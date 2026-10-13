use std::{mem::forget, os::fd::OwnedFd};

use crate::{
    unix_fd_stream::UnixFdStream,
    wl::{
        objects::{MessageHeader, WlObject, WlStr},
        wl_buffered_stream::WriteBuffer,
        wl_message_router::WlPendingId,
    },
};

pub struct WlMessageWriter<'a> {
    tx: &'a mut WriteBuffer,
    socket: &'a UnixFdStream,
    start_position: usize,
}

#[repr(transparent)]
pub struct WlMessageWriterGuard<'a>(WlMessageWriter<'a>);

impl<'a> WlMessageWriterGuard<'a> {
    #[inline(always)]
    pub fn pack_new_object_id(&mut self, local_id: &WlPendingId) -> std::io::Result<()> {
        self.pack_u32(local_id.id())?;
        Ok(())
    }

    #[inline(always)]
    pub fn pack_u32(&mut self, value: u32) -> std::io::Result<()> {
        self.make_room(4)?.copy_from_slice(&value.to_ne_bytes());
        self.0.tx.cursor += 4;
        Ok(())
    }

    #[inline(always)]
    pub fn pack_wl_str(&mut self, s: &WlStr) -> std::io::Result<()> {
        // the bytes in wl_str are already prefixed with the length, and there is null terminator at the end, so we can just copy them directly to the write buffer
        let len = s.wl_bytes.len() as u32;
        self.make_room(len as usize)?.copy_from_slice(s.wl_bytes);
        self.0.tx.cursor += len as usize;
        // we also need to ensure the string is 4 byte aligned by adding padding if necessary
        let padding = (4 - (s.wl_bytes.len() % 4)) % 4;
        self.make_room(padding)?.fill(0);
        self.0.tx.cursor += padding;
        Ok(())
    }

    #[inline(always)]
    pub fn pack_str(&mut self, s: &str) -> std::io::Result<()> {
        // we need to pack the string as len + bytes + null terminator and ensure it is 4 byte aligned. The len is the str bytes + the null terminator.
        let payload_len = s.len() + 1;
        let needed = 4 + payload_len.next_multiple_of(4);
        let out = self.make_room(needed)?;

        let len = payload_len as u32;
        out[..4].copy_from_slice(&len.to_ne_bytes());

        let bytes = &mut out[4..4 + payload_len];
        bytes[..s.len()].copy_from_slice(s.as_bytes());
        bytes[s.len()] = 0; // null terminator

        let padding = (4 - (payload_len % 4)) % 4;
        if padding > 0 {
            out[4 + payload_len..4 + payload_len + padding].fill(0);
        }

        self.0.tx.cursor += needed;
        Ok(())
    }

    #[inline(always)]
    pub fn end(self) -> std::io::Result<()> {
        self.commit(None)
    }

    pub fn end_with_fd(self, fd: OwnedFd) -> std::io::Result<()> {
        self.commit(Some(fd))
    }

    fn commit(mut self, fd: Option<OwnedFd>) -> std::io::Result<()> {
        if let Some(fd) = fd {
            if self.0.tx.fds.is_full() {
                let result = self.0.tx.flush_to(self.0.socket, self.0.start_position);
                self.0.start_position = 0;
                result?;
            }
            self.0.tx.fds.push_out_fd(fd)?;
        }

        let message_length = (self.0.tx.cursor - self.0.start_position) as u32;
        let word: &mut [u8; 4] = (&mut self.0.tx.buf
            [self.0.start_position + 4..self.0.start_position + 8])
            .try_into()
            .unwrap();
        *word = (u32::from_ne_bytes(*word) | message_length << 16).to_ne_bytes();

        // skip the destructor - this is our "commit".
        forget(self);
        Ok(())
    }

    #[inline(always)]
    fn begin<T: WlObject>(&mut self, op: T::Ops, type_id: u32) -> std::io::Result<()> {
        let opcode: u16 = op.into();

        let buf = self.make_room(8)?;
        buf[0..4].copy_from_slice(&type_id.to_ne_bytes());
        buf[4..8].copy_from_slice(&u32::from(opcode).to_ne_bytes());

        self.0.tx.cursor += MessageHeader::WL_HEADER_SIZE as usize;
        Ok(())
    }

    /// Ensure `n` more bytes fit after the cursor. If not, send every complete message
    /// before this one and move this message's partial bytes to the front.
    fn make_room(&mut self, n: usize) -> std::io::Result<&mut [u8]> {
        if self.0.tx.cursor + n <= self.0.tx.buf.len() {
            // SAFETY: We just checked that the slice is in bounds, so this is safe.
            return Ok(unsafe {
                self.0
                    .tx
                    .buf
                    .get_unchecked_mut(self.0.tx.cursor..self.0.tx.cursor + n)
            }); // happy path: same check as today
        }
        let start = self.0.start_position;
        if start == 0 {
            return Err(std::io::Error::other(
                "message does not fit in the write buffer",
            )); // > 4096 bytes on its own
        }
        let result = self.0.tx.flush_to(self.0.socket, start);
        self.0.start_position = 0;
        result?;
        if self.0.tx.cursor + n <= self.0.tx.buf.len() {
            // SAFETY: We just checked that the slice is in bounds, so this is safe.
            Ok(unsafe {
                self.0
                    .tx
                    .buf
                    .get_unchecked_mut(self.0.tx.cursor..self.0.tx.cursor + n)
            })
        } else {
            Err(std::io::Error::other(
                "message does not fit in the write buffer",
            ))
        }
    }
}

impl<'a> WlMessageWriter<'a> {
    pub(crate) fn new(buf: &'a mut WriteBuffer, socket: &'a UnixFdStream) -> Self {
        let start = buf.cursor;
        Self {
            tx: buf,
            socket,
            start_position: start,
        }
    }

    #[inline(always)]
    pub fn begin_message<T: WlObject>(
        self,
        op: T::Ops,
        type_id: u32,
    ) -> std::io::Result<WlMessageWriterGuard<'a>> {
        let mut guard = WlMessageWriterGuard(self);
        guard.begin::<T>(op, type_id)?;
        Ok(guard)
    }
}

impl Drop for WlMessageWriterGuard<'_> {
    fn drop(&mut self) {
        self.0.tx.cursor = self.0.start_position;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wl::objects::{
        wl_display::{DisplayOps, WlDisplay},
        wl_str_bytes,
    };
    use std::{io::Read, os::unix::net::UnixStream, time::Duration};

    const BUF_LEN: usize = 64;

    // struct Buffers {
    //     buf: [u8; BUF_LEN],
    //     cursor: usize,
    //     fds: OutFdBuffer,
    // }

    // impl Buffers {
    //     fn new() -> Self {
    //         // Non-zero fill so missing padding bytes would show up.
    //         Self {
    //             buf: [0xAA; BUF_LEN],
    //             cursor: 0,
    //             fds: OutFdBuffer::new(),
    //         }
    //     }

    //     // fn begin(&mut self) -> WlMessageWriterGuard<'_> {
    //     //     WlMessageWriter::new(&mut self.buf, &mut self.fds, &mut self.cursor)
    //     //         .begin_message::<WlDisplay>(DisplayOps::Sync, 1)
    //     //         .unwrap()
    //     // }

    //     fn written(&self) -> &[u8] {
    //         &self.buf[..self.cursor]
    //     }
    // }

    fn assert_peer_closed(mut peer: UnixStream) {
        // Times out instead of hanging if a copy of the other end is still open.
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut got = Vec::new();
        peer.read_to_end(&mut got).unwrap();
        assert!(got.is_empty());
    }

    // #[test]
    // fn header_second_word_is_size_high_opcode_low() {
    //     let mut b = Buffers::new();
    //     let mut msg = WlMessageWriter::new(&mut b.buf, &mut b.fds, &mut b.cursor)
    //         .begin_message::<WlDisplay>(DisplayOps::GetRegistry, 1)
    //         .unwrap();
    //     msg.pack_u32(2).unwrap();
    //     msg.end();
    //     assert_eq!(b.written()[..4], 1u32.to_ne_bytes());
    //     assert_eq!(b.written()[4..8], (12u32 << 16 | 1).to_ne_bytes());

    //     let mut header = [0u8; 8];
    //     header[..4].copy_from_slice(&7u32.to_ne_bytes());
    //     header[4..].copy_from_slice(&(20u32 << 16 | 3).to_ne_bytes());
    //     let parsed = MessageHeader::parse(&header);
    //     assert_eq!((parsed.object_id, parsed.opcode, parsed.size), (7, 3, 20));
    // }

    // #[test]
    // fn strings_are_length_prefixed_nul_terminated_and_padded() {
    //     for text in ["", "a", "abc", "abcd", "text/plain"] {
    //         let mut b = Buffers::new();
    //         let mut msg = b.begin();
    //         msg.pack_str(text).unwrap();
    //         msg.end();
    //         let encoded = &b.written()[8..];
    //         assert_eq!(
    //             encoded.len(),
    //             4 + (text.len() + 1).next_multiple_of(4),
    //             "{text:?}"
    //         );
    //         assert_eq!(encoded[..4], ((text.len() + 1) as u32).to_ne_bytes());
    //         assert_eq!(&encoded[4..4 + text.len()], text.as_bytes());
    //         assert!(
    //             encoded[4 + text.len()..].iter().all(|&b| b == 0),
    //             "{text:?}"
    //         );
    //     }
    // }

    // #[test]
    // fn compile_time_wl_str_matches_runtime_encoding() {
    //     let consts = [
    //         wl_str_bytes!("a"),
    //         wl_str_bytes!("abcd"),
    //         wl_str_bytes!("wl_seat"),
    //         wl_str_bytes!("ext_data_control_manager_v1"),
    //     ];
    //     for wl_str in &consts {
    //         let mut runtime = Buffers::new();
    //         let mut msg = runtime.begin();
    //         msg.pack_str(wl_str.str).unwrap();
    //         msg.end();

    //         let mut compiled = Buffers::new();
    //         let mut msg = compiled.begin();
    //         msg.pack_wl_str(wl_str).unwrap();
    //         msg.end();

    //         assert_eq!(compiled.written(), runtime.written(), "{}", wl_str.str);
    //     }
    // }

    // #[test]
    // fn a_string_that_exactly_fills_the_buffer_fits() {
    //     let mut b = Buffers::new();
    //     // Header (8) + length prefix (4) + text + NUL (1) == buffer length, no padding needed.
    //     let text = "x".repeat(BUF_LEN - 8 - 4 - 1);
    //     let mut msg = b.begin();
    //     msg.pack_str(&text).unwrap();
    //     msg.end();
    //     assert_eq!(b.cursor, BUF_LEN);
    // }

    // #[test]
    // fn a_string_that_does_not_fit_is_an_error_and_the_message_is_rolled_back() {
    //     let mut b = Buffers::new();
    //     let mut msg = b.begin();
    //     assert!(msg.pack_str(&"x".repeat(BUF_LEN)).is_err());
    //     drop(msg);
    //     assert_eq!(b.cursor, 0);
    // }

    // #[test]
    // fn a_header_that_does_not_fit_is_an_error_and_writes_nothing() {
    //     let mut b = Buffers::new();
    //     b.cursor = BUF_LEN - 4;
    //     let result = WlMessageWriter::new(&mut b.buf, &mut b.fds, &mut b.cursor)
    //         .begin_message::<WlDisplay>(DisplayOps::Sync, 1);
    //     assert!(result.is_err());
    //     drop(result);
    //     assert_eq!(b.cursor, BUF_LEN - 4);
    // }

    // #[test]
    // fn a_dropped_message_rolls_back_its_bytes_and_closes_its_fds() {
    //     let mut b = Buffers::new();
    //     let (peer, sent) = UnixStream::pair().unwrap();
    //     let mut msg = b.begin();
    //     msg.pack_u32(7).unwrap();
    //     msg.pack_fd(sent.into()).unwrap();
    //     drop(msg);

    //     assert_eq!(b.cursor, 0);
    //     assert!(b.fds.peek_out_fds().is_empty());
    //     assert_peer_closed(peer);
    // }

    // #[test]
    // fn rolling_back_a_message_keeps_the_ones_before_it() {
    //     let mut b = Buffers::new();
    //     let (_first_peer, first) = UnixStream::pair().unwrap();
    //     let (second_peer, second) = UnixStream::pair().unwrap();

    //     let mut msg = b.begin();
    //     msg.pack_fd(first.into()).unwrap();
    //     msg.end();
    //     let committed = b.cursor;

    //     let mut msg = b.begin();
    //     msg.pack_u32(7).unwrap();
    //     msg.pack_fd(second.into()).unwrap();
    //     drop(msg);

    //     assert_eq!(b.cursor, committed);
    //     assert_eq!(b.fds.peek_out_fds().len(), 1);
    //     assert_peer_closed(second_peer);
    // }
}
