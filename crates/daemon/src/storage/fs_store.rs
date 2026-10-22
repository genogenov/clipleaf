use std::{
    env,
    fs::{DirBuilder, File, OpenOptions},
    io::{self},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use clipleaf_core::log_debug;

const DEFAULT_DIR: &str = "XDG_STATE_HOME";
const HOME_DIR: &str = "HOME";

pub struct FsDataStore {
    base: PathBuf,
    payloads: Arc<Path>,
    lock: Option<File>,
}

impl FsDataStore {
    pub fn open_data_store_dir(dir: Option<&Path>) -> io::Result<Self> {
        let base = dir
            .map(PathBuf::from)
            .or_else(default_base)
            .or_else(fallback_base)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "could not open data store dir after trying {:?}, {} and {}",
                    dir, DEFAULT_DIR, HOME_DIR,
                ))
            })?;

        log_debug!("Built base dir: {:?}", base);

        let payloads = base.join("payloads");
        log_debug!("Payloads dir: {:?}", payloads);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&payloads)?;
        Ok(FsDataStore {
            base,
            payloads: Arc::from(payloads),
            lock: None,
        })
    }

    pub fn lock(&mut self) -> io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(self.base.join("lock"))?;
        match file.try_lock() {
            Ok(()) => {
                self.lock = Some(file);
                Ok(())
            }
            Err(std::fs::TryLockError::Error(e)) => Err(e),
            Err(std::fs::TryLockError::WouldBlock) => {
                Err(io::Error::other("another clipleafd already running"))
            }
        }
    }

    pub fn payloads_dir(&self) -> Arc<Path> {
        self.payloads.clone()
    }

    pub fn wal_dir(&self) -> PathBuf {
        self.base.join("wal.log")
    }

    // pub fn base_dir(&self) -> &Path {
    //     &self.base
    // }
}

fn default_base() -> Option<PathBuf> {
    let base = env::var_os(DEFAULT_DIR)?;
    log_debug!("Default base: {:?}", base);
    let p = Path::new(&base);
    if p.is_empty() || !p.is_absolute() {
        return None;
    }

    let result = PathBuf::from(p);

    Some(result.join("clipleaf"))
}

fn fallback_base() -> Option<PathBuf> {
    let base = env::var_os(HOME_DIR)?;
    log_debug!("Fallback base: {:?}", base);
    let p = Path::new(&base);
    if p.is_empty() || !p.is_absolute() {
        return None;
    }

    let result = PathBuf::from(p);
    Some(result.join(".local/state/clipleaf"))
}
