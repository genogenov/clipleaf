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
