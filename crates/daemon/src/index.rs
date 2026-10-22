use std::{
    cmp::max,
    collections::HashMap,
    io, mem,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use clipleaf_core::{log_debug, log_error};

use crate::storage::{
    Entry,
    wal::log::{LogMsg, Restored},
};

pub enum IndexMsg {
    Insert {
        seq: u64,
        time_ms: u64,
        entry: Entry,
    },
}

#[derive(Debug, Clone)]
pub struct Item {
    pub seq: u64,
    pub time_ms: u64,
    pub pinned: bool,
    pub entry: Arc<Entry>,
}
pub struct Snapshot {
    pub items: Vec<Item>,
}

#[derive(Clone)]
pub struct SnapshotHandle(Arc<Mutex<Arc<Snapshot>>>);

impl SnapshotHandle {
    // pub fn get_clone(&self) -> Arc<Snapshot> {
    //     self.0
    //         .lock()
    //         .unwrap_or_else(PoisonError::into_inner)
    //         .clone()
    // }

    fn get(&self) -> MutexGuard<'_, Arc<Snapshot>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Index {
    current: Arc<Snapshot>,
    by_key: HashMap<u128, u64>,
    history_count_limit: usize,
    pinned_count: usize,
    shared: SnapshotHandle,
    log_sender: Sender<LogMsg>,
}

impl Index {
    fn run(mut self, rx: Receiver<IndexMsg>) {
        while let Ok(msg) = rx.recv() {
            match msg {
                IndexMsg::Insert {
                    seq,
                    time_ms,
                    entry,
                } => self.insert(seq, time_ms, entry),
            }
        }
    }

    fn insert(&mut self, seq: u64, time_ms: u64, mut entry: Entry) {
        let mut items = self.current.items.clone();
        if let Some(id) = self.by_key.get(&entry.key) {
            // duplicate - move to front and drop the msg.
            if let Some(i) = items.iter().rposition(|x| x.entry.id == *id) {
                let mut item = items.remove(i);
                item.seq = max(item.seq, seq);
                item.time_ms = max(item.time_ms, time_ms);
                log_debug!(
                    "Copy matches entry {} - moved to top. Skipping entry item {:?}",
                    id,
                    item
                );
                items.insert(items.partition_point(|i| i.seq > item.seq), item);
                log_debug!("Total entries {:?}.", items.len());
                self.publish(items);
                return;
            } else {
                log_error!(
                    "Could not find id {id} in entries for duplicate - this is a bug. Inserting entry as new."
                )
            }
        }

        entry.commit();
        self.by_key.insert(entry.key, entry.id);
        let item = Item {
            seq,
            time_ms,
            pinned: false,
            entry: Arc::new(entry),
        };
        log_debug!("Added entry {:?}.", item);
        items.insert(
            // insert just behind the next biggest seq num.
            items.partition_point(|x| x.seq > seq),
            item.clone(),
        );
        if let Err(e) = self.log_sender.send(LogMsg::Append(item)) {
            log_error!("Failed sending message to WAL: {e}");
        }

        while items.len().saturating_sub(self.pinned_count) > self.history_count_limit {
            let Some(i) = items.iter().rposition(|it| !it.pinned) else {
                break;
            };

            let item = items.remove(i);
            if let Some(val) = self.by_key.get(&item.entry.key)
                && item.entry.id == *val
            {
                self.by_key.remove(&item.entry.key);
            }
            log_debug!("Evicting stale entry: {:?}", item);
            if let Err(e) = self.log_sender.send(LogMsg::Delete(item)) {
                log_error!("Failed sending message to WAL: {e}");
            }
        }

        log_debug!("Total entries {:?}.", items.len());
        self.publish(items);
    }

    fn publish(&mut self, items: Vec<Item>) {
        self.current = Arc::new(Snapshot { items });
        let old = mem::replace(&mut *self.shared.get(), self.current.clone());
        // make sure to drop the old mutex guard,
        // so if there are old entries running cleanups (e.g. drop files) we do not gold the lock and snapshot.get_clone() doesnt wait except for this small swap in place.
        drop(old);
    }
}

pub fn spawn_index(
    history_count_limit: usize,
    log_tx: Sender<LogMsg>,
    restored: Restored,
) -> io::Result<(Sender<IndexMsg>, SnapshotHandle)> {
    let (tx, rx) = mpsc::channel();

    let pinned_count = restored.items.iter().filter(|i| i.pinned).count();
    let current = Arc::new(Snapshot {
        items: restored.items,
    });
    let handle = SnapshotHandle(Arc::new(Mutex::new(current.clone())));
    let index = Index {
        by_key: restored.by_key,
        current,
        history_count_limit,
        shared: handle.clone(),
        pinned_count,
        log_sender: log_tx,
    };
    thread::Builder::new()
        .name("index".into())
        .spawn(move || index.run(rx))?;
    Ok((tx, handle))
}
