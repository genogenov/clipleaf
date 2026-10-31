use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub fn lock_file(path: &Path, mode: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(mode)
        .open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("Failed to open lock file {} : {}", path.display(), e)))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::Error(e)) => Err(io::Error::new(e.kind(), format!("Failed to lock file {} : {}", path.display(), e))),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::other("another clipleafd already running")),
    }
}
