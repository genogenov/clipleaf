use std::{
    fs::{self, DirBuilder, File},
    io::{self, ErrorKind},
    os::unix::{fs::DirBuilderExt, net::UnixListener},
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
};

use clipleaf_core::{ffi::event_fd::EventFd, log_debug, log_warn};

use crate::{
    ipc::{handler_v1::HandlerV1, listener::IpcListener},
    storage::lock_file,
};

pub struct IpcServer {
    _runtime_lock: File,
    shutdown: Arc<EventFd>,
    socket_path: PathBuf,
    listener: Option<JoinHandle<()>>,
    closed: bool,
}

impl IpcServer {
    pub fn init() -> io::Result<Self> {
        let socket_path = clipleaf_ipc::default_socket_path()?;
        let socket_dir = socket_path
            .parent()
            .ok_or_else(|| io::Error::other(format!("Invalid IPC server lock file path: {}", socket_path.display())))?;

        log_debug!("IPC Server: creating dir {}", socket_dir.display());

        match DirBuilder::new().mode(0o700).create(socket_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io::Error::new(e.kind(), format!("Error creating dir {} : {}", socket_dir.display(), e))),
        }

        let runtime_lock = socket_dir.join("lock");
        let lock = lock_file(&runtime_lock, 0o600)?;

        let shutdown = Arc::new(EventFd::new()?);

        Ok(Self {
            _runtime_lock: lock,
            listener: None,
            socket_path,
            shutdown,
            closed: false,
        })
    }

    pub fn start(&mut self) -> io::Result<()> {
        log_debug!("IPC Server: Deleting previous sock file at {}", self.socket_path.display());

        match fs::remove_file(&self.socket_path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(io::Error::other(format!("IPC Server: Failed to remove stale sock file: {e}")));
            }
            _ => {}
        };

        let listener = UnixListener::bind(&self.socket_path)?;
        listener.set_nonblocking(true)?;
        let mut listener = IpcListener::new(self.shutdown.clone(), listener, HandlerV1 {});
        let handle = thread::Builder::new().name("IPC-listener".to_owned()).spawn(move || {
            listener.run();
        })?;

        self.listener = Some(handle);
        Ok(())
    }

    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        if let Err(e) = self.shutdown.signal() {
            log_warn!("IPC shutdown: failed to signal shutdown: {e}");
        }
        if let Some(handle) = self.listener.take() {
            if let Err(e) = handle.join() {
                log_warn!("IPC shutdown: failed to join listener thread: {e:?}");
            }

            if let Err(e) = fs::remove_file(&self.socket_path) {
                log_warn!("IPC shutdown: failed to delete sock file: {e:?}");
            }
        }

        self.closed = true;
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}
