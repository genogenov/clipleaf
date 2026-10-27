use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::storage::wal::{encoder::MAX_RECORD_LEN, error::WalError};

pub struct WriteBuffer {
    pub(super) buff: Vec<u8>,
    pub(super) pos: usize,
}

pub struct RecordWriter<'a> {
    buffer: &'a mut WriteBuffer,
    start: usize,
}

impl<'a> RecordWriter<'a> {
    pub fn start(buffer: &'a mut WriteBuffer) -> Self {
        let start = buffer.buff.len();
        buffer.buff.extend_from_slice(&[0; 4]); // length placeholder, filled in by finish()
        Self { buffer, start }
    }

    pub fn finish_with_checksum_salt(self, salt: u64) -> Result<usize, WalError> {
        let len = self.buffer.buff.len() - self.start;
        let total = len + 8; // + checksum
        if total > MAX_RECORD_LEN {
            self.buffer.buff.truncate(self.start);
            return Err(WalError::RecordTooLarge);
        }
        self.buffer.buff[self.start..self.start + 4].copy_from_slice(&(len as u32).to_le_bytes());
        let hash = xxh3_64_with_seed(&self.buffer.buff[self.start..], salt);
        self.buffer.buff.extend_from_slice(&hash.to_le_bytes());
        self.buffer.pos += total;
        Ok(total)
    }

    pub fn len32_prefixed(&mut self, val: &[u8]) {
        let len = val.len() as u32;
        self.buffer.buff.extend(len.to_le_bytes().iter().chain(val));
    }

    pub fn bool(&mut self, val: bool) {
        self.buffer.buff.push(val as u8);
    }

    pub fn u8(&mut self, val: u8) {
        self.buffer.buff.push(val);
    }

    pub fn u16(&mut self, val: u16) {
        self.buffer.buff.extend_from_slice(&val.to_le_bytes());
    }

    pub fn u32(&mut self, val: u32) {
        self.buffer.buff.extend_from_slice(&val.to_le_bytes());
    }

    pub fn u64(&mut self, val: u64) {
        self.buffer.buff.extend_from_slice(&val.to_le_bytes());
    }

    pub fn u128(&mut self, val: u128) {
        self.buffer.buff.extend_from_slice(&val.to_le_bytes());
    }
}
