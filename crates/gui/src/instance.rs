use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::Duration,
};

use clipleaf_core::{log_debug, log_warn};
use clipleaf_default_gui::{GUI_CONTROL_MAX_LINE_LEN, GuiCommand, GuiControlError, GuiResponse, default_socket_path};

use crate::pubsub::Pub;

pub struct Instance;

impl Instance {
    pub fn acquire() -> io::Result<Option<(File, UnixListener, PathBuf)>> {
        let socket_path = default_socket_path()?;
        let socket_dir = socket_path
            .parent()
            .ok_or_else(|| io::Error::other(format!("Invalid GUI lock file path: {}", socket_path.display())))?;

        log_debug!("GUI Instance: creating dir {}", socket_dir.display());

        match DirBuilder::new().mode(0o700).create(socket_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("GUI: Error creating dir {} : {}", socket_dir.display(), e),
                ));
            }
        }

        let runtime_lock = socket_dir.join("lock");
        let Some(lock) = acquire_lock(&runtime_lock)? else {
            return Ok(None);
        };

        match fs::remove_file(&socket_path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(io::Error::other(format!("GUI Instance: Failed to remove stale sock file: {e}")));
            }
            _ => {}
        };

        Ok(Some((lock, UnixListener::bind(&socket_path)?, socket_path)))
    }

    pub fn listen(listener: UnixListener, sender: Pub<GuiCommand>) -> io::Result<JoinHandle<()>> {
        thread::Builder::new().name("gui-control".into()).spawn(move || {
            Self::run_listen_loop(listener, sender);
        })
    }

    fn run_listen_loop(listener: UnixListener, sender: Pub<GuiCommand>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(e) = Self::handle_connection(stream, sender.clone()) {
                        log_warn!("GUI Instance: Error while handling connection: {e}. Ignoring and accepting new connection.");
                    }
                }
                Err(e) => {
                    log_debug!("GUI Instance: Error while accepting connection: {e}. Ignoring.");
                    thread::sleep(Duration::from_millis(100));
                }
            };
        }
    }

    fn handle_connection(mut socket: UnixStream, sender: Pub<GuiCommand>) -> Result<(), GuiControlError> {
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        socket.set_write_timeout(Some(Duration::from_millis(100)))?;

        let mut buf = [0u8; GUI_CONTROL_MAX_LINE_LEN];
        let mut pos = 0;
        loop {
            if pos >= GUI_CONTROL_MAX_LINE_LEN {
                return write_err_best_effort(&mut socket, GuiControlError::InvalidData);
            }

            match socket.read(&mut buf[pos..]) {
                Ok(0) => return write_err_best_effort(&mut socket, GuiControlError::InvalidData),
                Ok(n) => pos += n,
                Err(e) => match e.kind() {
                    ErrorKind::Interrupted => {}
                    ErrorKind::UnexpectedEof => return Err(GuiControlError::InvalidData),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut => {
                        return Err(GuiControlError::Io(io::Error::new(
                            e.kind(),
                            "GUI Control: Timeout while waiting for response",
                        )));
                    }
                    _ => {
                        return Err(GuiControlError::Io(io::Error::new(
                            e.kind(),
                            format!("GUI Control: Failed to read response from socket: {e}"),
                        )));
                    }
                },
            }

            if let Some(res) = GuiCommand::parse_line(&buf[..pos]) {
                sender.publish(res)?;
                return write_success_best_effort(&mut socket);
            }

            if buf[..pos].contains(&b'\n') {
                return write_err_best_effort(&mut socket, GuiControlError::InvalidData);
            }
        }
    }
}

fn write_success_best_effort(socket: &mut UnixStream) -> Result<(), GuiControlError> {
    if let Err(e) = socket.write_all(GuiResponse::CommandOk.as_sock_line()) {
        log_warn!("GUI Control: Failed to send success response to socket: {e}. Ignoring.");
    }

    Ok(())
}

fn write_err_best_effort(socket: &mut UnixStream, err: GuiControlError) -> Result<(), GuiControlError> {
    if let Err(e) = socket.write_all(GuiResponse::Rejected.as_sock_line()) {
        log_warn!("GUI Control: Failed to send error response to socket: {e}. Ignoring.");
    }

    Err(err)
}

fn acquire_lock(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("GUI Instance: Failed to open lock file {} : {}", path.display(), e)))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::Error(e)) => Err(io::Error::new(
            e.kind(),
            format!("GUI Instance: Failed to lock file {} : {}", path.display(), e),
        )),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Shutdown;

    use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};

    use super::*;
    use crate::pubsub::{self, Sub};

    /// Sends `request` (then shuts down our write side if `eof`), runs the handler on the other
    /// end, and returns (handler result is Ok, bytes the handler wrote back, commands published).
    fn handle(request: &[u8], eof: bool) -> (bool, Vec<u8>, Vec<GuiCommand>) {
        let (mut peer, gui_end) = UnixStream::pair().unwrap();
        peer.write_all(request).unwrap();
        if eof {
            peer.shutdown(Shutdown::Write).unwrap();
        }
        let (tx, rx): (_, Sub<GuiCommand>) = pubsub::channel(8).unwrap();
        let ok = Instance::handle_connection(gui_end, tx).is_ok();
        let mut reply = Vec::new();
        peer.read_to_end(&mut reply).unwrap(); // the handler dropped its end: EOF
        (ok, reply, published(&rx))
    }

    /// Everything published so far. `recv_all` starts with a blocking eventfd read, so only call
    /// it when poll says the doorbell rang; otherwise nothing was published.
    fn published(rx: &Sub<GuiCommand>) -> Vec<GuiCommand> {
        let mut fds = [PollFd::new(rx.fd(), PollEvents::IN)];
        let mut got = Vec::new();
        if let PollResult::Ready(_) = poll(&mut fds, Some(Duration::ZERO)).expect("poll") {
            rx.recv_all(|c| got.push(c));
        }
        got
    }

    #[test]
    fn valid_command_is_published_and_acknowledged() {
        assert_eq!(handle(b"show\n", false), (true, b"ok\n".to_vec(), vec![GuiCommand::Show]));
        assert_eq!(handle(b"toggle\n", true), (true, b"ok\n".to_vec(), vec![GuiCommand::Toggle]));
    }

    #[test]
    fn unknown_line_is_rejected_and_not_published() {
        assert_eq!(handle(b"garbage\n", false), (false, b"rejected\n".to_vec(), vec![]));
    }

    #[test]
    fn eof_before_newline_is_rejected_and_not_published() {
        assert_eq!(handle(b"show", true), (false, b"rejected\n".to_vec(), vec![]));
    }

    #[test]
    fn line_of_max_len_without_newline_is_rejected() {
        let long = [b'a'; GUI_CONTROL_MAX_LINE_LEN];
        assert_eq!(handle(&long, false), (false, b"rejected\n".to_vec(), vec![]));
    }
}
