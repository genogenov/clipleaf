use std::{
    cmp::{self, max},
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, RecvError, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::{log_debug, log_error, log_info, log_warn};

use crate::{
    index::Item,
    storage::{
        Data, Entry, RESTORE_ID_LIMIT, entry,
        wal::{
            encoder::{
                HEADER_LEN, MAX_RECORD_LEN, RestoreItem, RestoreOp, encode_append, encode_delete,
                encode_thumb, encode_touch,
            },
            error::WalError,
            record_reader::WalReader,
            wal_file::WalFile,
        },
    },
};

#[derive(Debug)]
struct LogItem {
    pub item: Item,
    pub size_on_disk: u32,
}

const LOG_FILE_SYNC_DELAY: Duration = Duration::from_secs(1);
const COMPACT_ON_MIN_DEAD_RATIO: f32 = 1.0;
const MIN_DEAD_SIZE_FOR_COMPACT: u64 = 1024 * 1024;
const WAL_FILE_NAME: &str = "wal.log";
const TMP_WAL_FILE_NAME: &str = "wal.log.tmp";

#[derive(Debug)]
pub enum LogMsg {
    Append(Item),
    Delete(Item),
    Touch { id: u64, seq: u64, time_ms: u64 },
    Thumb { item: Item, type_index: u32 },
}

pub struct Restored {
    pub items: Vec<Item>,
    pub by_key: HashMap<u128, u64>,
    pub next_seq: u64,
}

pub struct WriteAheadLog {
    wal_file: WalFile,
    base: Arc<Path>,
    payloads: Arc<Path>,
    last_written_without_sync: Option<Instant>,
    logged_failures: i32,
    processing_failures: i32,
    has_pending_trim: bool,
    has_pending_compact: bool,
    has_pending_dir_sync: bool,
    fail_logging_threshold: i32,
    live_size: u64,
    live_records: HashMap<u64, LogItem>,
    next_min_seq: u64,
}

// TODO: split, Result<..,WalError> control flow

impl WriteAheadLog {
    pub fn open_restore(base_dir: Arc<Path>, payloads: Arc<Path>) -> io::Result<(Self, Restored)> {
        let tmp_wal_path = base_dir.join(TMP_WAL_FILE_NAME);
        match fs::remove_file(&tmp_wal_path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                log_warn!("Failed to remove stale temp WAL file: {e}")
            }
            _ => {}
        };

        let wal_path = base_dir.join(WAL_FILE_NAME);
        let mut reader = WalFile::open_or_create(&wal_path, MAX_RECORD_LEN * 5)?;

        let (by_id, max_seq_seen) = Self::read_all_records(&mut reader);
        let wal_file = reader.finish()?;

        let next_min_seq = wal_file.header().seq_snapshot;
        let mut wal = Self {
            wal_file,
            base: base_dir,
            payloads,
            fail_logging_threshold: 5,
            logged_failures: 0,
            processing_failures: 0,
            has_pending_trim: false,
            has_pending_compact: false,
            has_pending_dir_sync: false,
            last_written_without_sync: None,
            live_records: HashMap::new(),
            live_size: 0,
            next_min_seq,
        };

        let restored = Self::restore(&mut wal, by_id, max_seq_seen)?;

        Ok((wal, restored))
    }

    pub fn compact_if_necessary(&mut self) -> io::Result<bool> {
        let dead_bytes = self.wal_file.len() - HEADER_LEN as u64 - self.live_size;
        let dead_to_live_ratio = dead_bytes as f32 / self.live_size as f32;

        let should_compact = self.has_pending_compact
            || (dead_to_live_ratio >= COMPACT_ON_MIN_DEAD_RATIO
                && dead_bytes >= MIN_DEAD_SIZE_FOR_COMPACT);
        if should_compact {
            log_debug!(
                "WAL: dead to live ratio: {dead_to_live_ratio}, dead size:{dead_bytes}, has pending compact: {}. Starting compact..",
                self.has_pending_compact
            );
            self.compact()?;
        }
        Ok(should_compact)
    }

    fn compact(&mut self) -> io::Result<()> {
        let tmp_wal_path = self.base.join(TMP_WAL_FILE_NAME);

        if let Err(e) = self.write_tmp_wal_and_swap(&tmp_wal_path) {
            if let Err(re) = fs::remove_file(&tmp_wal_path) {
                log_warn!("failed to remove {}: {re}", tmp_wal_path.display());
            }
            return Err(e);
        }

        Ok(())
    }

    fn write_tmp_wal_and_swap(&mut self, tmp_wal_path: &Path) -> io::Result<()> {
        let mut new_wal = WalFile::create_or_overwrite(tmp_wal_path, self.next_min_seq)?;
        let mut new_live_map = HashMap::with_capacity(self.live_records.capacity());
        let mut new_live_size: u64 = 0;
        let flush_threshold = MAX_RECORD_LEN * 2;
        for item in self.live_records.values() {
            let salt = new_wal.header().salt;
            match encode_append(&item.item, new_wal.begin_record(), salt) {
                Ok(len) => {
                    new_live_map.insert(
                        item.item.entry.id,
                        LogItem {
                            item: item.item.clone(),
                            size_on_disk: len as u32,
                        },
                    );
                    new_live_size += len as u64;
                }
                Err(e) => {
                    log_warn!("Failed to write entry to WAL during compact, skipping: {e}");
                }
            }
            new_wal.flush_if_over(flush_threshold)?;
        }

        new_wal.flush()?;
        new_wal.sync()?;

        fs::rename(tmp_wal_path, self.base.join(WAL_FILE_NAME))?;
        self.wal_file = new_wal;
        self.last_written_without_sync = None;
        self.live_records = new_live_map;
        self.live_size = new_live_size;
        self.has_pending_compact = false;
        self.has_pending_trim = false;
        self.processing_failures = 0;

        if let Err(e) = self.sync_dir() {
            log_warn!("Failed dir sync after compact. Will retry on next log file sync: {e}");
            self.has_pending_dir_sync = true;
            self.last_written_without_sync = Some(Instant::now());
        }
        Ok(())
    }

    fn read_all_records(reader: &mut WalReader) -> (HashMap<u64, RestoreItem>, u64) {
        let mut by_id: HashMap<u64, RestoreItem> = HashMap::new();

        let mut max_seq_seen = 0;
        while let Some(entry_res) = reader.next() {
            let entry = match entry_res {
                Ok(tuple) => tuple,
                Err(e) => match e {
                    WalError::BadIdentifier
                    | WalError::UnsupportedVersion(_)
                    | WalError::Corrupted
                    | WalError::RecordTooLarge => {
                        log_error!("Failed reading entry - skipping rest. Error: {e}");
                        break;
                    }
                    WalError::UnknownType((_, t)) => {
                        log_warn!("Encountered unknown entry type {t} - skipping.");
                        continue;
                    }
                    WalError::Io(error) => {
                        log_error!("Failed reading entry with I/O error: {error}. Skipping rest.",);
                        break;
                    }
                },
            };

            match entry.op {
                RestoreOp::Insert(item) => {
                    max_seq_seen = max(max_seq_seen, max(item.seq, item.id));

                    if let Some(old) = by_id.insert(item.id, item) {
                        log_warn!("Duplicate ID in WAL: {}", old.id);
                    }
                }
                RestoreOp::Delete { id } => {
                    by_id.remove(&id);
                }
                RestoreOp::Touch { id, seq, time_ms } => {
                    max_seq_seen = max(max_seq_seen, max(seq, id));
                    by_id.entry(id).and_modify(|e| {
                        e.seq = max(e.seq, seq);
                        e.time_ms = max(e.time_ms, time_ms);
                    });
                }
                RestoreOp::Thumb {
                    id,
                    type_index,
                    meta,
                } => {
                    let Some(item) = by_id.get_mut(&id) else {
                        log_warn!("Could not find entry with ID {id} for Thumb record");
                        continue;
                    };
                    let Some(t) = item.types.get_mut(type_index as usize) else {
                        log_warn!(
                            "Could not find type  with id {type_index} in entry with ID {id} for Thumb record"
                        );
                        continue;
                    };
                    t.meta = Some(meta);
                    item.size_on_disk += entry.size_on_disk;
                }
            }
        }

        (by_id, max_seq_seen)
    }

    fn restore(
        &mut self,
        mut by_id: HashMap<u64, RestoreItem>,
        max_seq_seen: u64,
    ) -> io::Result<Restored> {
        log_debug!("Begin WAL restore..");
        // let mut buff = Vec::new();
        //let bytes = self.wal_file.read_to_end(&mut buff)?;

        let next_seq = max_seq_seen.saturating_add(1);
        self.next_min_seq = max(self.wal_file.header().seq_snapshot, next_seq);
        if self.next_min_seq >= RESTORE_ID_LIMIT {
            log_warn!(
                "history ids/seqs out of range ({}): starting with an empty history. This is probably a bug.",
                self.next_min_seq
            );
            self.wal_file.reset()?;
            by_id.clear();
            self.next_min_seq = self.wal_file.header().seq_snapshot;
        }

        let len = by_id.len();
        log_debug!("Total entries to restore: {}", len);
        let mut sorted = Vec::new();
        let mut paths_accounted_for = HashSet::new();

        for val in by_id.into_values() {
            let mut payloads_valid = true;
            let mut valid_paths = Vec::new();
            for (file_type, tdata) in val.types.iter().filter_map(|t| match &t.data {
                entry::Data::File(p) => Some((p, t)),
                _ => None,
            }) {
                let path = file_type.payload_path(&self.payloads, val.id);
                match fs::metadata(&path) {
                    Ok(metadata) if metadata.size() != tdata.size => {
                        log_warn!(
                            "payload for entry id {} and file {} failed size check. Dropping entry",
                            val.id,
                            path.display()
                        );
                        payloads_valid = false;
                        break;
                    }
                    Ok(_) => {
                        valid_paths.push(path);
                    }
                    Err(e) => {
                        log_error!(
                            "Failed to check file metadata for entry id {} and file {}. Dropping entry. Error: {}",
                            val.id,
                            path.display(),
                            e
                        );
                        payloads_valid = false;
                        break;
                    }
                }
            }

            if payloads_valid {
                sorted.push(val);
                paths_accounted_for.extend(valid_paths);
            } else {
                log_warn!(
                    "Skipping entry {} because payload coult not be found",
                    val.id
                )
            }
        }

        for direntry in fs::read_dir(&self.payloads)? {
            let entry = direntry?;
            if !paths_accounted_for.contains(&entry.path()) {
                log_warn!("Deleting unknown file {}", entry.path().display());
                let ftype = entry.file_type()?;
                if ftype.is_file() {
                    if let Err(e) = fs::remove_file(entry.path()) {
                        log_error!("Failed to remove file : {e}");
                    }
                } else if ftype.is_dir()
                    && let Err(e) = fs::remove_dir_all(entry.path())
                {
                    log_error!("Failed to remove dir : {e}");
                }
            }
        }

        sorted.sort_unstable_by_key(|a| cmp::Reverse(a.seq));

        let mut by_key = HashMap::with_capacity(len);
        let mut items = Vec::with_capacity(len);
        for item in sorted.into_iter() {
            let entry = Entry::restored(item.id, item.types, self.payloads.clone());
            by_key.entry(entry.key).or_insert(entry.id);
            let entry = Arc::from(entry);
            let history_item = Item {
                entry,
                pinned: item.pinned,
                seq: item.seq,
                time_ms: item.time_ms,
            };
            self.live_records.insert(
                history_item.entry.id,
                LogItem {
                    item: history_item.clone(),
                    size_on_disk: item.size_on_disk,
                },
            );
            self.live_size += item.size_on_disk as u64;
            items.push(history_item);
        }

        log_debug!("Restored {} entries.", items.len());
        Ok(Restored {
            items,
            by_key,
            next_seq: self.next_min_seq,
        })
    }

    pub fn start(self) -> io::Result<Sender<LogMsg>> {
        let (tx, rx) = mpsc::channel();

        thread::Builder::new()
            .name("WAL".into())
            .spawn(move || self.run(rx))?;
        Ok(tx)
    }

    fn run(mut self, rx: Receiver<LogMsg>) {
        if let Err(e) = self.compact_if_necessary() {
            self.handle_processing_result(Err(e));
        }

        while let Ok(msg) = self.receive_msg(&rx) {
            log_debug!("Processing WAL msg: {:?}", msg);
            let res = self.handle_msg(msg);
            self.handle_processing_result(res);
            if let Err(e) = self.compact_if_necessary() {
                self.handle_processing_result(Err(e));
            }
        }

        // disconnected
        if self.last_written_without_sync.is_some() {
            self.sync_state_files();
        }
    }

    fn handle_processing_result(&mut self, res: io::Result<()>) {
        match res {
            Err(e) => {
                self.log_msg_processing_error(e);
                self.processing_failures += 1;
                if !self.has_pending_trim && !self.wal_file.try_trim() {
                    self.has_pending_trim = true;
                }
            }
            Ok(()) => {
                if self.logged_failures > 0 {
                    log_info!(
                        "WAL writes recovered after {} failed records",
                        self.logged_failures
                    );
                    self.logged_failures = 0;
                    if self.processing_failures > 0 {
                        self.has_pending_compact = true;
                    }
                    self.processing_failures = 0;
                }
            }
        }
    }

    fn log_msg_processing_error(&mut self, e: io::Error) {
        if self.logged_failures < self.fail_logging_threshold {
            log_error!("An error occurred while processing WAL msg: {e}");
        } else if self.logged_failures == self.fail_logging_threshold {
            log_error!(
                "An error occurred while processing WAL msg, and errors count reached the configured threshold, silencing until reccovery: {e}"
            );
        }
        self.logged_failures += 1;
    }

    fn receive_msg(&mut self, rx: &Receiver<LogMsg>) -> Result<LogMsg, RecvError> {
        loop {
            let Some(last) = self.last_written_without_sync else {
                return rx.recv();
            };

            let timeout = LOG_FILE_SYNC_DELAY.saturating_sub(last.elapsed());
            if timeout.is_zero() {
                self.sync_state_files();
                continue;
            }
            match rx.recv_timeout(timeout) {
                Ok(val) => return Ok(val),
                Err(RecvTimeoutError::Timeout) => {
                    self.sync_state_files();
                }
                Err(RecvTimeoutError::Disconnected) => return Err(RecvError),
            }
        }
    }

    fn sync_state_files(&mut self) {
        let mut should_rearm = false;

        if let Err(e) = self.wal_file.sync() {
            self.log_msg_processing_error(e);
            should_rearm = true;
            // if syncdata fails, the kernel might mark the pages clean,
            // and the next time we try sync_data it might succeed, even tho the pages are dropped and not written.
            self.has_pending_compact = true;
            if let Err(e) = self.compact_if_necessary() {
                self.handle_processing_result(Err(e));
            }
        } else {
            log_debug!("Synced WAL log file");
        }

        if self.has_pending_dir_sync {
            if let Err(e) = self.sync_dir() {
                self.log_msg_processing_error(e);
                should_rearm = true;
            } else {
                self.has_pending_dir_sync = false;
                log_debug!("Synced WAL dir");
            }
        }

        self.last_written_without_sync = should_rearm.then(Instant::now);
    }

    fn sync_dir(&mut self) -> io::Result<()> {
        OpenOptions::new().read(true).open(&self.base)?.sync_all()
    }

    fn handle_pending_trim(&mut self) -> io::Result<()> {
        if self.has_pending_trim {
            if self.wal_file.try_trim() {
                log_info!("Recovered after pending trim");
                self.has_pending_trim = false;
            } else {
                return Err(io::Error::other(
                    "WAL has a failing pending trim, skipping message.",
                ));
            }
        }

        Ok(())
    }

    fn handle_msg(&mut self, msg: LogMsg) -> Result<(), io::Error> {
        match &msg {
            LogMsg::Append(record) => self.append(record),
            LogMsg::Delete(record) => self.delete(record),
            LogMsg::Touch { id, seq, time_ms } => self.touch(*id, *seq, *time_ms),
            LogMsg::Thumb { item, type_index } => self.thumb(item, *type_index),
        }
    }

    fn append(&mut self, record: &Item) -> io::Result<()> {
        log_debug!(
            "Processing WAL append entry id {}, seq: {}",
            record.entry.id,
            record.seq
        );

        // skip encode append if syncing fails - no history storage.
        log_debug!("syncing entry payload files..");
        self.sync_payload_files(record)?;

        let salt = self.wal_file.header().salt;
        let len =
            encode_append(record, self.wal_file.begin_record(), salt).map_err(io::Error::other)?;
        self.live_records.insert(
            record.entry.id,
            LogItem {
                item: record.clone(),
                size_on_disk: 0,
            },
        );

        self.write_to_log()?;
        if let Some(l) = self.live_records.get_mut(&record.entry.id) {
            l.size_on_disk = len as u32;
        }
        self.live_size += len as u64;
        self.next_min_seq = max(
            self.next_min_seq,
            record.seq.max(record.entry.id).saturating_add(1),
        );
        log_debug!("Processed WAL append - written {} bytes", len);
        Ok(())
    }

    fn delete(&mut self, entry: &Item) -> Result<(), io::Error> {
        log_debug!("Processing WAL delete..");
        let salt = self.wal_file.header().salt;
        let len = encode_delete(entry.entry.id, self.wal_file.begin_record(), salt)
            .map_err(io::Error::other)?;

        let item = self.live_records.remove(&entry.entry.id);
        self.live_size -= item.map_or_default(|i| i.size_on_disk) as u64;

        let res = self.write_to_log();

        log_debug!("Dropping entry files..");
        entry.entry.drop_files();
        res?;
        log_debug!("Processed WAL delete - written {} bytes", len);
        Ok(())
    }

    fn touch(&mut self, id: u64, seq: u64, time_ms: u64) -> Result<(), io::Error> {
        let Some(log_item) = self.live_records.get_mut(&id) else {
            log_debug!(
                "Touch for entry {id} not in the log (its payload sync probably failed), skipped"
            );
            return Ok(());
        };

        let salt = self.wal_file.header().salt;
        let len = encode_touch(id, seq, time_ms, self.wal_file.begin_record(), salt)
            .map_err(io::Error::other)?;

        log_item.item.seq = seq;
        log_item.item.time_ms = time_ms;

        self.write_to_log()?;
        self.next_min_seq = max(self.next_min_seq, seq.max(id).saturating_add(1));
        log_debug!("Processed WAL touch - written {} bytes", len);
        Ok(())
    }

    fn thumb(&mut self, item: &Item, type_index: u32) -> Result<(), io::Error> {
        if !self.live_records.contains_key(&item.entry.id) {
            log_debug!("Thumb for entry {} not in the log, skipped", item.entry.id);
            return Ok(());
        }
        let Some(t) = item.entry.types.get(type_index as usize) else {
            log_debug!(
                "Thumb for entry {} and type index {} not in the log, skipped",
                item.entry.id,
                type_index
            );
            return Ok(());
        };

        let Some(ref meta) = t.meta else {
            log_debug!(
                "Meta for entry {} and type index {} not set, skipped thumb record",
                item.entry.id,
                type_index
            );
            return Ok(());
        };

        let salt = self.wal_file.header().salt;
        let len = encode_thumb(
            item.entry.id,
            type_index,
            meta,
            self.wal_file.begin_record(),
            salt,
        )
        .map_err(io::Error::other)?;

        self.write_to_log()?;
        let Some(log_item) = self.live_records.get_mut(&item.entry.id) else {
            log_debug!("Thumb for entry {} not in the log, skipped", item.entry.id);
            return Ok(());
        };
        log_item.item = item.clone();
        log_item.size_on_disk += len as u32;
        self.live_size += len as u64;
        log_debug!("Processed WAL thumb - written {} bytes", len);
        Ok(())
    }

    fn write_to_log(&mut self) -> Result<(), io::Error> {
        if let Err(e) = self.handle_pending_trim() {
            self.wal_file.discard_pending_buffer();
            return Err(e);
        }
        self.wal_file.flush()?;
        self.last_written_without_sync
            .get_or_insert_with(Instant::now);
        Ok(())
    }

    fn sync_payload_files(&self, record: &Item) -> io::Result<()> {
        for f in record.entry.types.iter().filter_map(|t| match t.data {
            Data::File(ref f) => Some(f),
            _ => None,
        }) {
            File::open(f.payload_path(&self.payloads, record.entry.id))?.sync_data()?
        }
        Ok(())
    }
}
