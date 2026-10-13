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
    //! Unit tests for what the public API cannot show: the exact encoding of arguments the
    //! session never sends in this shape (u32 + string + u32 across an overflow), the compile-time
    //! `wl_str` constants, and the state of the write buffer after a failed flush. Everything else
    //! about the writer is covered through the session in `core/tests/raw_writer.rs`.
    use super::*;
    use crate::{
        fd_io::OutFdBuffer,
        wl::{
            objects::{
                wl_data_offer::WlDataControlOffer,
                wl_data_source::WlDataControlSource,
                wl_display::{DisplayOps, WlDisplay},
                wl_registry::{RegistryOps, WlRegistry},
                wl_str_bytes,
            },
            wl_message_reader::WlMessageReader,
        },
    };
    use std::{
        io::{ErrorKind, Read},
        os::unix::net::UnixStream,
    };

    const BUF_LEN: usize = 4096;

    // A write buffer wired to a socket whose other end the test can read.
    struct Harness {
        tx: WriteBuffer,
        sock: UnixFdStream,
        peer: Option<UnixStream>,
    }

    impl Harness {
        fn new() -> Self {
            let (a, b) = UnixStream::pair().unwrap();
            Self {
                // Non-zero fill so missing padding bytes would show up.
                tx: WriteBuffer {
                    buf: [0xAA; BUF_LEN],
                    cursor: 0,
                    fds: OutFdBuffer::new(),
                },
                sock: a.into(),
                peer: Some(b),
            }
        }

        fn writer(&mut self) -> WlMessageWriter<'_> {
            WlMessageWriter::new(&mut self.tx, &self.sock)
        }

        fn offer(&mut self, id: u32, mime: &str) -> std::io::Result<()> {
            WlDataControlSource { local_id: id }
                .offer(WlMessageWriter::new(&mut self.tx, &self.sock), mime)
        }

        fn flush(&mut self) {
            self.tx.flush(&self.sock).unwrap();
        }

        // Whatever the peer has received so far (sends are synchronous on unix sockets).
        fn received(&mut self) -> Vec<u8> {
            let peer = self.peer.as_mut().unwrap();
            peer.set_nonblocking(true).unwrap();
            let mut out = Vec::new();
            let mut chunk = [0u8; BUF_LEN];
            loop {
                match peer.read(&mut chunk) {
                    Ok(0) => return out,
                    Ok(n) => out.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return out,
                    Err(e) => panic!("{e}"),
                }
            }
        }

        // The (object id, opcode, body) of each whole message in `bytes`.
        fn messages(bytes: &[u8]) -> Vec<(u32, u16, Vec<u8>)> {
            let mut out = Vec::new();
            let mut rest = bytes;
            while !rest.is_empty() {
                let header = MessageHeader::parse(rest.first_chunk::<8>().expect("torn header"));
                let size = header.size as usize;
                assert!(size >= 8 && size <= rest.len(), "torn message");
                out.push((header.object_id, header.opcode, rest[8..size].to_vec()));
                rest = &rest[size..];
            }
            out
        }

        fn written(&self) -> &[u8] {
            &self.tx.buf[..self.tx.cursor]
        }
    }

    fn wl_str(text: &str) -> Vec<u8> {
        let mut out = ((text.len() + 1) as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(text.as_bytes());
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    // Fills the buffer with whole 100-byte `offer`s up to `target` (a multiple of 4 and of
    // whatever remains: the last one is shortened, never below 16 bytes).
    fn fill_to(h: &mut Harness, target: usize) {
        let mut id = 1000;
        while h.tx.cursor < target {
            let left = target - h.tx.cursor;
            assert!(left >= 16, "cannot fill {left} bytes with whole messages");
            let size = match left {
                ..=100 => left,
                101..=115 => left - 16,
                _ => 100,
            };
            h.offer(id, &"z".repeat(size - 13)).unwrap();
            id += 1;
        }
        assert_eq!(h.tx.cursor, target);
    }

    #[test]
    fn header_second_word_is_size_high_opcode_low() {
        let mut h = Harness::new();
        let mut msg = h
            .writer()
            .begin_message::<WlDisplay>(DisplayOps::GetRegistry, 1)
            .unwrap();
        msg.pack_u32(2).unwrap();
        msg.end().unwrap();
        assert_eq!(h.written()[..4], 1u32.to_ne_bytes());
        assert_eq!(h.written()[4..8], (12u32 << 16 | 1).to_ne_bytes());
    }

    #[test]
    fn strings_are_length_prefixed_nul_terminated_and_padded() {
        for text in ["", "a", "abc", "abcd", "text/plain"] {
            let mut h = Harness::new();
            let mut msg = h
                .writer()
                .begin_message::<WlDisplay>(DisplayOps::Sync, 1)
                .unwrap();
            msg.pack_str(text).unwrap();
            msg.end().unwrap();
            assert_eq!(&h.written()[8..], wl_str(text), "{text:?}");
        }
    }

    #[test]
    fn compile_time_wl_str_matches_runtime_encoding() {
        let consts = [
            wl_str_bytes!("a"),
            wl_str_bytes!("abcd"),
            wl_str_bytes!("wl_seat"),
            wl_str_bytes!("ext_data_control_manager_v1"),
        ];
        for text in &consts {
            let mut h = Harness::new();
            let mut msg = h
                .writer()
                .begin_message::<WlDisplay>(DisplayOps::Sync, 1)
                .unwrap();
            msg.pack_wl_str(text).unwrap();
            msg.end().unwrap();
            assert_eq!(&h.written()[8..], wl_str(text.str), "{}", text.str);
        }
    }

    #[test]
    fn wl_str_and_u32_arguments_survive_an_overflow_between_them() {
        // The same layout as `wl_registry.bind`: u32, string, u32, u32. The session never builds
        // a message like this after start-up, so only a unit test can push it across the edge.
        for text in [
            wl_str_bytes!("wl_output"),
            wl_str_bytes!("ext_data_control_manager_v1"),
        ] {
            for remaining in (0..=60).step_by(4) {
                let mut h = Harness::new();
                fill_to(&mut h, BUF_LEN - remaining);
                let mut msg = h
                    .writer()
                    .begin_message::<WlRegistry>(RegistryOps::Bind, 2)
                    .unwrap();
                msg.pack_u32(54).unwrap();
                msg.pack_wl_str(&text).unwrap();
                msg.pack_u32(1).unwrap();
                msg.pack_u32(0xdead_beef).unwrap();
                msg.end().unwrap();
                h.flush();

                let messages = Harness::messages(&h.received());
                let (id, _, body) = messages.last().unwrap();
                let mut r = WlMessageReader::new(body);
                assert_eq!(
                    (*id, r.u32(), r.str(), r.u32(), r.u32()),
                    (2, Some(54), Some(text.bytes), Some(1), Some(0xdead_beef)),
                    "{} with {remaining} left",
                    text.str
                );
            }
        }
    }

    #[test]
    fn a_send_error_during_an_overflow_flush_leaves_the_buffer_clean() {
        let mut h = Harness::new();
        let (_reader, fd) = UnixStream::pair().unwrap();
        WlDataControlOffer::receive(0xff00_0000, h.writer(), "a/first", fd.into()).unwrap();
        fill_to(&mut h, BUF_LEN - 20);
        h.peer = None;

        // Header written, the string does not fit, the flush hits a closed peer (EPIPE).
        let err = h.offer(7, &"m".repeat(43)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err}");

        // Nothing stale: no cursor left behind and no queued fds.
        assert_eq!(h.tx.cursor, 0);
        assert!(h.tx.fds.peek_out_fds().is_empty());
    }

    #[test]
    fn a_send_error_during_a_commit_time_flush_leaves_the_buffer_clean() {
        let mut h = Harness::new();
        let mut keep = Vec::new();
        for i in 0..28 {
            let (reader, fd) = UnixStream::pair().unwrap();
            keep.push(reader);
            WlDataControlOffer::receive(0xff00_0000, h.writer(), &format!("x/{i}"), fd.into())
                .unwrap();
        }
        h.peer = None;

        // The 29th fd does not fit, so the commit flushes first and the flush fails.
        let (reader, fd) = UnixStream::pair().unwrap();
        keep.push(reader);
        let err =
            WlDataControlOffer::receive(0xff00_0000, h.writer(), "x/last", fd.into()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err}");

        assert_eq!(h.tx.cursor, 0);
        assert!(h.tx.fds.peek_out_fds().is_empty());
    }
}
