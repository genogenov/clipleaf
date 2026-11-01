use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, ErrorKind},
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

use clipleaf_core::log_debug;
use clipleaf_default_gui::default_socket_path;

#[allow(dead_code)]
pub struct Instance {
    _lock: File,
    socket_path: PathBuf,
    listener: UnixListener,
}

impl Instance {
    pub fn ackuire() -> io::Result<Option<Self>> {
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
        let Some(lock) = ackuire_lock(&runtime_lock)? else {
            return Ok(None);
        };

        match fs::remove_file(&socket_path) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(io::Error::other(format!("GUI Instance: Failed to remove stale sock file: {e}")));
            }
            _ => {}
        };

        Ok(Some(Instance {
            _lock: lock,
            listener: UnixListener::bind(&socket_path)?,
            socket_path,
        }))
    }
}

fn ackuire_lock(path: &Path) -> io::Result<Option<File>> {
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
