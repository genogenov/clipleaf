use std::{
    cmp::{self, max},
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    hash::{BuildHasher, Hasher, RandomState},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use clipleaf_core::{log_debug, log_error, log_info, log_warn};

use crate::{
    index::Item,
    storage::{
        Entry, entry,
        wal::{
            encoder::{
                HEADER_LEN, Header, MAX_RECORD_LEN, RestoreItem, RestoreRecord, decode_header,
                decode_record, encode_append, encode_delete, encode_header,
            },
            error::WalError,
        },
    },
};

#[derive(Debug)]
pub enum LogMsg {
    Append(Item),
    Delete(Item),
}

pub struct Restored {
    pub items: Vec<Item>,
    pub by_key: HashMap<u128, u64>,
    pub next_seq: u64,
}

pub struct WriteAheadLog {
    log_file: File,
    // wal_path: PathBuf,
    payloads: Arc<Path>,
    header: Header,
    last_good_len: u64,
}

impl WriteAheadLog {
    pub fn open(wal_path: PathBuf, payloads: Arc<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(&wal_path)?;

        let header = {
            let mut buff = [0u8; HEADER_LEN];
            match file.read_exact(&mut buff) {
                Ok(_) => {
                    log_debug!("Start WAL header decode..");
                    match decode_header(&buff) {
                        Ok(h) => Ok(h),
                        Err(e) => {
                            log_error!("Error while decoding WAL header: {e}");
                            match e {
                                WalError::BadIdentifier
                                | WalError::UnsupportedVersion(_)
                                | WalError::SmallEncodeBuffer
                                | WalError::UnknownType(_) => {
                                    log_warn!(
                                        "WAL Error during header parsing, resetting. Error: {e}"
                                    );
                                    reset_wal(&mut file)
                                }
                                WalError::Corrupted => reset_wal(&mut file),
                            }
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    if let Ok(m) = file.metadata()
                        && m.size() != 0
                    {
                        // skip log if its an empty file => probably first run.
                        log_warn!("Failed to read WAL file, resetting. Error: {e}");
                    }
                    reset_wal(&mut file)
                }
                Err(e) => {
                    log_error!("Failed to read WAL file - exiting.: {e}");
                    Err(e)
                }
            }
        }?;
        let pos = file.stream_position()?;
        Ok(Self {
            // wal_path: wal_path,
            log_file: file,
            payloads,
            header,
            last_good_len: pos,
        })
    }

    pub fn restore(&mut self) -> io::Result<Restored> {
        log_debug!("Begin WAL restore..");
        let mut buff = Vec::new();
        let bytes = self.log_file.read_to_end(&mut buff)?;
        let mut by_id: HashMap<u64, RestoreItem> = HashMap::new();

        let mut offset: usize = 0;
        let mut max_seq_seen = 0;
        while offset < bytes {
            let (read, entry) = match decode_record(&buff[offset..], self.header.salt) {
                Ok(tuple) => tuple,
                Err(e) => match e {
                    WalError::BadIdentifier
                    | WalError::UnsupportedVersion(_)
                    | WalError::Corrupted
                    | WalError::SmallEncodeBuffer => {
                        let new_len = HEADER_LEN + offset;
                        log_error!(
                            "Failed reading entry. Rewinding to last known good len {}. Error: {e}",
                            new_len
                        );
                        self.log_file.set_len(new_len as u64)?;
                        self.log_file.seek(SeekFrom::End(0))?;
                        log_debug!("WAL reverted to len {}", new_len);
                        break;
                    }
                    WalError::UnknownType((read, t)) => {
                        log_warn!("Encountered unknown entry type {t} - skipping.");
                        offset += read;
                        continue;
                    }
                },
            };
            offset += read;

            match entry {
                RestoreRecord::Insert(item) => {
                    max_seq_seen = max(max_seq_seen, max(item.seq, item.id));

                    if let Some(old) = by_id.insert(item.id, item) {
                        log_warn!("Duplicate ID in WAL: {}", old.id);
                    }
                }
                RestoreRecord::Delete { id } => {
                    by_id.remove(&id);
                }
            }
        }
        self.last_good_len += offset as u64;

        let next_seq = match max_seq_seen.checked_add(1) {
            Some(next) => next,
            None => {
                log_warn!(
                    "history log has an impossible seq ({max_seq_seen}), starting with empty history"
                );
                self.header = reset_wal(&mut self.log_file)?;
                self.last_good_len = HEADER_LEN as u64;
                by_id.clear();
                0
            }
        };

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
                let path = file_type.payload_path(&self.payloads);
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
            items.push(Item {
                entry: Arc::from(entry),
                pinned: item.pinned,
                seq: item.seq,
                time_ms: item.time_ms,
            });
        }

        log_debug!("Restored {} entries.", items.len());
        Ok(Restored {
            items,
            by_key,
            next_seq: max(self.header.seq_snapshot, next_seq),
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
        let mut write_buf = vec![0u8; MAX_RECORD_LEN as usize];
        let mut failures = 0;
        let fail_threshold = 5;
        let mut has_pending_trim = false;
        while let Ok(msg) = rx.recv() {
            log_debug!("Processing WAL msg: {:?}", msg);
            let res = self.handle_msg(&mut write_buf, &mut has_pending_trim, msg);

            match res {
                Err(e) => {
                    if failures < fail_threshold {
                        log_error!("An error occurred while processing WAL msg: {e}");
                    } else if failures == fail_threshold {
                        log_error!(
                            "An error occurred while processing WAL msg, and errors count reached the configured threshold, silencing until reccovery: {e}"
                        );
                    }
                    failures += 1;
                    if !has_pending_trim && !self.try_trim() {
                        has_pending_trim = true;
                    }
                }
                Ok(()) if failures > 0 => {
                    log_warn!("WAL writes recovered after {} failed records", failures);
                    failures = 0;
                }
                Ok(()) => {}
            }
        }
    }

    fn handle_msg(
        &mut self,
        write_buf: &mut [u8],
        has_pending_trim: &mut bool,
        msg: LogMsg,
    ) -> Result<(), io::Error> {
        if *has_pending_trim {
            if self.try_trim() {
                log_info!("Recovered after pending trim");
                *has_pending_trim = false;
            } else {
                if let LogMsg::Delete(item) = &msg {
                    log_warn!("Dropping files for Delete request while WAL trim is failing.");
                    item.entry.drop_files();
                }
                return Err(io::Error::other(
                    "WAL has a failing pending trim, skipping message.",
                ));
            }
        }
        match &msg {
            LogMsg::Append(record) => self.append(record, write_buf),
            LogMsg::Delete(record) => self.delete(record, write_buf),
        }
    }

    fn try_trim(&mut self) -> bool {
        self.log_file.set_len(self.last_good_len).is_ok()
    }

    fn append(&mut self, record: &Item, buf: &mut [u8]) -> io::Result<()> {
        log_debug!(
            "Processing WAL append entry id {}, seq: {}",
            record.entry.id,
            record.seq
        );
        let len = encode_append(record, self.header.salt, buf).map_err(io::Error::other)?;
        self.log_file.write_all(&buf[..len])?;
        self.last_good_len += len as u64;
        log_debug!("Processed WAL append - written {} bytes", len);
        Ok(())
    }

    fn delete(&mut self, entry: &Item, buf: &mut [u8]) -> Result<(), io::Error> {
        log_debug!("Processing WAL delete..");
        let len = encode_delete(entry.entry.id, self.header.salt, buf).map_err(io::Error::other)?;
        let res = self.log_file.write_all(&buf[..len]);

        if res.is_ok() {
            self.last_good_len += len as u64;
        }

        log_debug!("Dropping entry files..");
        entry.entry.drop_files();
        res?;
        log_debug!("Processed WAL delete - written {} bytes", len);
        Ok(())
    }
}

fn random_salt() -> u64 {
    RandomState::new().build_hasher().finish()
}

fn reset_wal(file: &mut File) -> io::Result<Header> {
    file.set_len(0)?;
    file.rewind()?;
    write_new_header(file)
}

fn write_new_header(file: &mut File) -> io::Result<Header> {
    log_debug!("Writing new WAL header..");
    let mut buff = [0u8; HEADER_LEN];
    let header = Header {
        salt: random_salt(),
        seq_snapshot: 1,
    };
    encode_header(&header, &mut buff);
    file.write_all(&buff)?;
    Ok(header)
}
