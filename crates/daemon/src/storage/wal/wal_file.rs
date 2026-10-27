use std::{
    fs::{self, File, OpenOptions},
    hash::{BuildHasher, Hasher, RandomState},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use clipleaf_core::{log_debug, log_error, log_warn};

use crate::storage::wal::{
    encoder::{HEADER_LEN, Header, MAX_RECORD_LEN, decode_header, encode_header},
    error::WalError,
    wal_writer::{WalWriter, WriteBuffer},
};

pub struct WalFile {
    file: File,
    header: Header,
    len: u64,
    write_buff: WriteBuffer,
}

impl WalFile {
    pub fn create_or_overwrite(path: &Path, seq_snapshot: u64) -> io::Result<Self> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
            _ => {}
        };
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(path)?;

        let header = Header {
            salt: random_salt(),
            seq_snapshot,
        };
        let header_buf = &mut [0; HEADER_LEN];
        encode_header(&header, header_buf);
        file.write_all(header_buf)?;
        let pos = file.stream_position()?;

        Ok(Self {
            file,
            len: pos,
            header,
            write_buff: WriteBuffer {
                buff: Vec::with_capacity(MAX_RECORD_LEN),
                pos: 0,
            },
        })
    }

    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .mode(0o600)
            .open(path)?;

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
                                | WalError::RecordTooLarge
                                | WalError::UnknownType(_) => {
                                    log_warn!(
                                        "WAL Error during header parsing, resetting. Error: {e}"
                                    );
                                    Self::reset_wal(&mut file, &mut buff)
                                }
                                WalError::Corrupted => Self::reset_wal(&mut file, &mut buff),
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
                    Self::reset_wal(&mut file, &mut buff)
                }
                Err(e) => {
                    log_error!("Failed to read WAL file - exiting.: {e}");
                    Err(e)
                }
            }
        }?;
        let pos = file.stream_position()?;

        Ok(Self {
            file,
            len: pos,
            header,
            write_buff: WriteBuffer {
                buff: Vec::with_capacity(MAX_RECORD_LEN),
                pos: 0,
            },
        })
    }

    pub fn begin_record(&mut self) -> WalWriter<'_> {
        self.write_buff.buff.truncate(self.write_buff.pos);
        WalWriter::start(&mut self.write_buff)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        let res = self
            .file
            .write_all(&self.write_buff.buff[..self.write_buff.pos]);
        if res.is_ok() {
            self.len += self.write_buff.pos as u64;
        }
        self.write_buff.buff.clear();
        self.write_buff.pos = 0;
        res
    }

    pub fn discard_pending_buffer(&mut self) {
        self.write_buff.buff.clear();
        self.write_buff.pos = 0;
    }

    pub fn flush_if_over(&mut self, threshold: usize) -> io::Result<()> {
        if self.write_buff.pos >= threshold {
            self.flush()
        } else {
            Ok(())
        }
    }

    pub fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let res = self.file.read_to_end(buf)?;
        self.len += res as u64;
        Ok(res)
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    pub fn set_position(&mut self, pos: u64) -> io::Result<u64> {
        self.file.set_len(pos)?;
        let res = self.file.seek(SeekFrom::End(0));
        if res.is_ok() {
            self.len = pos;
        }
        res
    }

    pub fn try_trim(&mut self) -> bool {
        self.file.set_len(self.len).is_ok()
    }

    pub fn reset(&mut self) -> io::Result<()> {
        self.header = Self::reset_wal(&mut self.file, &mut [0; HEADER_LEN])?;
        self.discard_pending_buffer();
        self.len = HEADER_LEN as u64;
        Ok(())
    }

    fn reset_wal(file: &mut File, buf: &mut [u8; HEADER_LEN]) -> io::Result<Header> {
        file.set_len(0)?;
        file.rewind()?;

        log_debug!("Writing new WAL header..");
        let header = Header {
            salt: random_salt(),
            seq_snapshot: 1,
        };

        encode_header(&header, buf);
        file.write_all(buf)?;
        Ok(header)
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn len(&self) -> u64 {
        self.len
    }
}

fn random_salt() -> u64 {
    RandomState::new().build_hasher().finish()
}
