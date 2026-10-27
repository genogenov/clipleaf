use std::{
    fs::File,
    io::{self, ErrorKind, Read, Seek},
};

use clipleaf_core::{log_debug, log_warn};

use crate::storage::wal::{
    encoder::{Header, MAX_RECORD_LEN, RestoreRecord, decode_record},
    error::WalError,
    wal_file::WalFile,
};

pub struct WalReader {
    file: File,
    header: Header,
    buf: Vec<u8>,
    start: usize,
    offset: u64,
    filled: usize,
    initial_position: u64,
    eof: bool,
    done: bool,
}

impl WalReader {
    pub fn new(mut file: File, header: Header, max_alloc: usize) -> io::Result<Self> {
        let pos = file.stream_position()?;
        Ok(Self {
            buf: vec![0; max_alloc.max(MAX_RECORD_LEN)],
            eof: false,
            done: false,
            file,
            header,
            offset: 0,
            start: 0,
            filled: 0,
            initial_position: pos,
        })
    }

    pub fn finish(self) -> io::Result<WalFile> {
        let position = self.initial_position + self.offset;
        let file_len = self.file.metadata()?.len();
        if position < file_len {
            log_warn!(
                "WAL cut at offset {position} of {file_len} bytes: {} bytes of history dropped",
                file_len - position
            );
            self.file.set_len(position)?;
        }
        Ok(WalFile::new(self.file, self.header, position))
    }

    pub fn next(&mut self) -> Option<Result<RestoreRecord, WalError>> {
        if self.done {
            return None;
        }
        loop {
            let available = &self.buf[self.start..self.filled];
            if let Some(len_bytes) = available.first_chunk::<4>() {
                let len = u32::from_le_bytes(*len_bytes);
                let total = len as usize + 8;
                if len < 5 || total > MAX_RECORD_LEN {
                    self.done = true;
                    return Some(Err(WalError::Corrupted));
                }
                if available.len() >= total {
                    let rec =
                        decode_record(&available[..total], len, total as u32, self.header.salt);
                    self.start += total;
                    if rec.is_ok() || matches!(rec, Err(WalError::UnknownType(_))) {
                        self.offset += total as u64;
                    } else {
                        self.done = true;
                    }
                    return Some(rec);
                }
            }
            if self.eof {
                return None;
            }
            // keep the partial record, move it to the front, then read more after it
            self.buf.copy_within(self.start.., 0);
            self.filled -= self.start;
            // self.buf.truncate(self.buf.len() - self.start);
            self.start = 0;

            // self.buf.resize(MAX_RECORD_LEN as usize, 0);
            match self.file.read(&mut self.buf[self.filled..]) {
                Ok(n) => {
                    log_debug!("read {n} bytes from stream");
                    self.filled += n;
                    self.eof = n == 0;
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(WalError::Io(e)));
                }
            }
        }
    }
}
