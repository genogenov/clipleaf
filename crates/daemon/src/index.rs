use std::{
    cmp::max,
    collections::HashMap,
    io, mem,
    path::Path,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
};

use clipleaf_core::{ContentKind, log_debug, log_error, log_warn};

use crate::{
    image::{ImageJob, ImageSource},
    storage::{
        Data, Entry, TypeMeta,
        wal::log::{LogMsg, Restored},
    },
    work_queue::WorkQueue,
};

pub enum IndexMsg {
    Insert {
        seq: u64,
        time_ms: u64,
        entry: Entry,
        done_signal: SyncSender<()>,
    },
    SetThumb {
        id: u64,
        type_index: u32,
        meta: TypeMeta,
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
    pub fn get_clone(&self) -> Arc<Snapshot> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

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
    img_queue: Arc<WorkQueue<ImageJob>>,
    payload_dir: Arc<Path>,
}

impl Index {
    fn run(mut self, rx: Receiver<IndexMsg>) {
        // queue jobs for all items that currently do not have thumbs before we start indexing
        for item in self.current.items.iter().rev() {
            for job in self.image_jobs(&item.entry) {
                self.img_queue.push(job);
            }
        }

        while let Ok(msg) = rx.recv() {
            match msg {
                IndexMsg::Insert {
                    seq,
                    time_ms,
                    entry,
                    done_signal,
                } => self.insert(seq, time_ms, entry, done_signal),
                IndexMsg::SetThumb {
                    id,
                    type_index,
                    meta,
                } => self.set_thumb(id, type_index, meta),
            }
        }
    }

    fn insert(&mut self, seq: u64, time_ms: u64, mut entry: Entry, done_signal: SyncSender<()>) {
        let mut items = self.current.items.clone();
        if let Some(id) = self.by_key.get(&entry.key) {
            // duplicate - move to front and drop the msg.
            if let Some(i) = items.iter().rposition(|x| x.entry.id == *id) {
                let mut item = items.remove(i);
                item.seq = max(item.seq, seq);
                item.time_ms = max(item.time_ms, time_ms);
                // log_debug!(
                //     "Copy matches entry {} - moved to top. Skipping entry item {:?}",
                //     id,
                //     item
                // );
                if let Err(e) = self.log_sender.send(LogMsg::Touch {
                    id: item.entry.id,
                    seq: item.seq,
                    time_ms: item.time_ms,
                }) {
                    log_error!("Failed sending touch message to WAL: {e}");
                }
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
        let jobs: Vec<_> = self.image_jobs(&item.entry).collect();
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

        for imgjob in jobs {
            log_debug!(
                "Pushing Img processing job entry ID {} and type idx {}",
                imgjob.id,
                imgjob.type_index
            );
            self.img_queue.push(imgjob);
        }
        let _ = done_signal.try_send(());
    }

    fn image_jobs<'a>(&'a self, entry: &'a Entry) -> impl Iterator<Item = ImageJob> + 'a {
        entry
            .types
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.meta.is_none()
                    && ContentKind::from_mime(&t.mime).is_some_and(|k| k == ContentKind::Image)
            })
            .map(|(i, t_img)| ImageJob {
                id: entry.id,
                type_index: i as u32,
                source: match t_img.data {
                    Data::Inline(ref items) => ImageSource::Inline(items.clone()),
                    Data::File(ref payload_file) => {
                        ImageSource::File(payload_file.payload_path(&self.payload_dir, entry.id))
                    }
                },
            })
    }

    fn set_thumb(&mut self, id: u64, type_index: u32, meta: TypeMeta) {
        let Some(pos) = self.current.items.iter().position(|i| i.entry.id == id) else {
            log_debug!(
                "Item with ID {id} not found in current items - possibly evicted - dropping set thumb msg"
            );
            return;
        };

        let mut items = self.current.items.clone();

        let Some(item) = items.get_mut(pos) else {
            return;
        };
        let Some(entry) = item
            .entry
            .clone_with_meta_as_committed(type_index as usize, meta)
        else {
            log_warn!(
                "Could not clone entry with ID {id} and type index {type_index} for set_thumb - dropping set thumb msg"
            );
            return;
        };

        *item = Item {
            seq: item.seq,
            time_ms: item.time_ms,
            pinned: item.pinned,
            entry: Arc::new(entry),
        };

        if let Err(e) = self.log_sender.send(LogMsg::Thumb {
            item: item.clone(),
            type_index,
        }) {
            log_error!("Failed sending message to WAL: {e}");
        }

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
    img_queue: Arc<WorkQueue<ImageJob>>,
    payload_dir: Arc<Path>,
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
        img_queue,
        payload_dir,
    };

    thread::Builder::new()
        .name("index".into())
        .spawn(move || index.run(rx))?;
    Ok((tx, handle))
}
