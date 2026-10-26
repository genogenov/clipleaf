use std::{
    env,
    ffi::OsString,
    io::{self, ErrorKind},
    os::fd::AsFd,
    path::{Path, PathBuf},
};

pub type WlWireMessage<'a> = (MessageHeader, &'a [u8], &'a mut InFdBuffer);
pub type NextMessageResult<'a> = std::io::Result<WlWireMessage<'a>>;

use crate::{
    fd_io::{InFdBuffer, OutFdBuffer},
    log_debug,
    unix_fd_stream::{ReadMode, UnixFdStream, WriteMode},
    wl::{objects::MessageHeader, wl_message_writer::WlMessageWriter},
};

pub struct WlBufferedStream {
    socket: UnixFdStream,
    rx: ReadBuffer,
    tx: WriteBuffer,
}

struct ReadBuffer {
    buf: [u8; 4096],
    cursor: usize,
    filled: usize,
    fds: InFdBuffer,
}

pub(crate) struct WriteBuffer {
    pub(crate) buf: [u8; 4096],
    pub(crate) cursor: usize,
    pub(crate) fds: OutFdBuffer,
    pub(crate) mode: WriteMode,
}

// Borrowed halves of a WlBufferedStream: an event from the reader can stay alive while the writer queues requests.
pub struct WlStreamReader<'a> {
    socket: &'a UnixFdStream,
    rx: &'a mut ReadBuffer,
}

pub struct WlStreamWriter<'a> {
    socket: &'a UnixFdStream,
    tx: &'a mut WriteBuffer,
}

fn socket_path(
    display_path: Option<OsString>,
    runtime_dir: Option<OsString>,
) -> io::Result<PathBuf> {
    let display_path_or_default = display_path.unwrap_or_else(|| OsString::from("wayland-0"));
    let display = Path::new(&display_path_or_default);
    if display.is_absolute() {
        return Ok(display.to_path_buf());
    }

    let runtime_dir = runtime_dir
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;

    Ok(Path::new(&runtime_dir).join(display))
}

impl AsFd for WlBufferedStream {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

impl WlBufferedStream {
    pub fn connect(socket_path: &Path) -> std::io::Result<Self> {
        Ok(Self::new(UnixFdStream::connect(socket_path)?))
    }

    pub fn connect_from_env() -> std::io::Result<Self> {
        let socket_path = socket_path(
            env::var_os("WAYLAND_DISPLAY"),
            env::var_os("XDG_RUNTIME_DIR"),
        )?;
        log_debug!("Wayland socket path: {}", socket_path.display());

        Self::connect(&socket_path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot connect to {}: {e}", socket_path.display()),
            )
        })
    }

    pub(crate) fn new(socket: UnixFdStream) -> Self {
        Self {
            socket,
            rx: ReadBuffer {
                buf: [0u8; 4096],
                cursor: 0,
                filled: 0,
                fds: InFdBuffer::new(),
            },
            tx: WriteBuffer {
                buf: [0u8; 4096],
                cursor: 0,
                fds: OutFdBuffer::new(),
                mode: WriteMode::Block,
            },
        }
    }

    pub fn split(&mut self) -> (WlStreamReader<'_>, WlStreamWriter<'_>) {
        (
            WlStreamReader {
                socket: &self.socket,
                rx: &mut self.rx,
            },
            WlStreamWriter {
                socket: &self.socket,
                tx: &mut self.tx,
            },
        )
    }

    pub fn set_nonblocking_writes(&mut self) {
        self.tx.mode = WriteMode::NonBlock;
    }

    pub fn has_pending_writes(&self) -> bool {
        self.tx.cursor > 0
    }

    #[cfg(test)]
    pub(crate) fn read_next_message(&mut self, mode: ReadMode) -> NextMessageResult<'_> {
        self.rx.next_message(&self.socket, mode)
    }

    pub fn write(&mut self) -> std::io::Result<usize> {
        self.tx.flush(&self.socket)
    }

    pub fn get_writer(&mut self) -> WlMessageWriter<'_> {
        self.tx.message_writer(&self.socket)
    }
}

impl WlStreamReader<'_> {
    pub(crate) fn read_next_message(&mut self, mode: ReadMode) -> NextMessageResult<'_> {
        self.rx.next_message(self.socket, mode)
    }
}

impl WlStreamWriter<'_> {
    pub fn write(&mut self) -> std::io::Result<usize> {
        self.tx.flush(self.socket)
    }

    pub fn write_blocking(&mut self) -> std::io::Result<usize> {
        self.tx.flush_blocking(self.socket)
    }

    pub fn get_writer(&mut self) -> WlMessageWriter<'_> {
        self.tx.message_writer(self.socket)
    }
}

impl ReadBuffer {
    fn next_message<'a>(
        &'a mut self,
        socket: &UnixFdStream,
        mode: ReadMode,
    ) -> NextMessageResult<'a> {
        loop {
            if let Some(h) = self.buf[self.cursor..self.filled].first_chunk::<8>() {
                let header = MessageHeader::parse(h);

                if header.size > self.buf.len() as u16
                    || header.size < MessageHeader::WL_HEADER_SIZE
                {
                    return Err(std::io::Error::other(format!(
                        "Message size {} invalid",
                        header.size
                    )));
                }
                // Only return the message once all of it is in the buffer; otherwise fall
                // through and read more bytes from the socket.
                if self.cursor + header.size as usize <= self.filled {
                    let message_body_offset = self.cursor + MessageHeader::WL_HEADER_SIZE as usize;
                    self.cursor += header.size as usize;
                    return Ok((
                        header,
                        &self.buf[message_body_offset..self.cursor],
                        &mut self.fds,
                    ));
                }
            }

            // we may have read a partial message, so we need to move the remaining bytes to the beginning of the buffer
            let mut remaining_bytes = 0;
            if self.cursor < self.filled {
                remaining_bytes = self.filled - self.cursor;
                self.buf.copy_within(self.cursor..self.filled, 0);
            }
            self.filled -= self.cursor;
            self.cursor = 0;

            let new_bytes_read =
                socket.read(&mut self.buf[remaining_bytes..], &mut self.fds, mode)?;
            if new_bytes_read == 0 {
                // EOF reached, no more messages to read.. if we have remaining bytes it means we have a partial message that we cant parse.
                if remaining_bytes > 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "EOF reached with partial message in buffer",
                    ));
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Compositor closed the connection - EOF reached",
                ));
            }

            self.filled += new_bytes_read;
        }
    }
}

impl WriteBuffer {
    pub(crate) fn flush(&mut self, socket: &UnixFdStream) -> std::io::Result<usize> {
        if self.cursor == 0 {
            return Ok(0);
        }
        self.flush_to_with_mode(socket, self.cursor, self.mode)
    }

    pub(crate) fn flush_blocking(&mut self, socket: &UnixFdStream) -> std::io::Result<usize> {
        if self.cursor == 0 {
            return Ok(0);
        }
        self.flush_to_with_mode(socket, self.cursor, WriteMode::Block)
    }

    fn message_writer<'a>(&'a mut self, socket: &'a UnixFdStream) -> WlMessageWriter<'a> {
        WlMessageWriter::new(self, socket)
    }

    pub(crate) fn flush_to(&mut self, socket: &UnixFdStream, n: usize) -> std::io::Result<usize> {
        self.flush_to_with_mode(socket, n, self.mode)
    }

    fn flush_to_with_mode(
        &mut self,
        socket: &UnixFdStream,
        n: usize,
        mode: WriteMode,
    ) -> std::io::Result<usize> {
        if n > self.cursor {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Cannot flush more bytes than are in the buffer",
            ));
        }
        // log_debug!("Flushing {} bytes to the stream", n);
        let sent = socket.write(&self.buf[..n], self.fds.peek_out_fds(), mode)?;
        if sent > 0 {
            self.fds.clear_and_close_out_fds(); // they went with the first byte
        }
        self.buf.copy_within(sent..self.cursor, 0);
        self.cursor -= sent;
        Ok(sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_joins_the_display_name_to_the_runtime_dir() {
        let dir = || Some(OsString::from("/run/user/1000"));
        assert_eq!(
            socket_path(None, dir()).unwrap(),
            Path::new("/run/user/1000/wayland-0")
        );
        assert_eq!(
            socket_path(Some("wayland-1".into()), dir()).unwrap(),
            Path::new("/run/user/1000/wayland-1")
        );
    }

    #[test]
    fn an_absolute_display_is_used_as_is() {
        for dir in [None, Some(OsString::from("/run/user/1000"))] {
            assert_eq!(
                socket_path(Some("/tmp/sway/wayland-5".into()), dir).unwrap(),
                Path::new("/tmp/sway/wayland-5")
            );
        }
    }

    #[test]
    fn a_relative_display_needs_the_runtime_dir() {
        for display in [None, Some(OsString::from("wayland-1"))] {
            let err = socket_path(display, None).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::NotFound);
            assert!(err.to_string().contains("XDG_RUNTIME_DIR"), "{err}");
        }
    }
}
