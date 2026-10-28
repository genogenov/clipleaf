use std::{
    cmp::max,
    io::{self, Read},
};

use crate::protocol::{
    error::ProtocolError,
    header::{HEADER_LEN, Header},
};

pub const MIN_FRAME_BUFFER_SIZE: usize = HEADER_LEN + 1024;
pub const SHRINK_FRAME_BUFFER_THRESHOLD_FACTOR: usize = 2;

pub struct FrameReader {
    buf: Vec<u8>,
    start: usize,
    end: usize,
    max_frame_len: usize,
    default_buffer_size: usize,
    pending_frame_len: usize,
}

impl FrameReader {
    pub fn new(default_buffer_size: usize, max_frame_len: usize) -> FrameReader {
        let default = max(default_buffer_size, MIN_FRAME_BUFFER_SIZE);
        Self {
            buf: vec![0; default],
            start: 0,
            end: 0,
            max_frame_len: max(max_frame_len, MIN_FRAME_BUFFER_SIZE),
            default_buffer_size: default,
            pending_frame_len: 0,
        }
    }

    pub fn next_frame(&mut self) -> Option<Result<(Header, &[u8]), ProtocolError>> {
        let available = &self.buf[self.start..self.end];
        if let Some(h) = available.first_chunk::<HEADER_LEN>() {
            let header = Header::decode(h);
            if header.len as usize > self.max_frame_len - HEADER_LEN {
                return Some(Err(ProtocolError::FrameTooBig));
            }

            let total = header.len as usize + HEADER_LEN;

            if available.len() < total {
                self.pending_frame_len = total;
                return None;
            }
            let body = &self.buf[self.start + HEADER_LEN..self.start + total];
            self.start += total;
            self.pending_frame_len = 0;
            return Some(Ok((header, body)));
        }

        None
    }

    pub fn fill_from(&mut self, read: &mut impl Read) -> io::Result<usize> {
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }

        let needed = max(self.default_buffer_size, self.pending_frame_len).max(self.end + 1);
        if self.buf.len() < needed {
            self.buf.resize(needed, 0);
        } else if self.buf.len() > needed * SHRINK_FRAME_BUFFER_THRESHOLD_FACTOR {
            self.buf.truncate(needed);
            self.buf.shrink_to_fit();
        }

        let n = read.read(&mut self.buf[self.end..])?;
        self.end += n;
        Ok(n)
    }
}
