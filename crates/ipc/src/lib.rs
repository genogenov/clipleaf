//! Client and wire types for the clipleaf daemon IPC.
//!
//! Protocol specification: <https://github.com/genogenov/clipleaf/blob/main/crates/ipc/PROTOCOL.md>

#![forbid(unsafe_code)]

use std::{env, ffi::OsString, io, path::PathBuf};

pub mod client;
pub mod frame;
pub mod protocol;

pub const RELATIVE_SOCKET_PATH: &str = "clipleaf/daemon.sock";

pub fn default_socket_path() -> io::Result<PathBuf> {
    socket_path(env::var_os("XDG_RUNTIME_DIR"))
}

pub fn socket_path(runtime_dir: Option<OsString>) -> io::Result<PathBuf> {
    let dir = runtime_dir
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set or not an absolute path"))?;
    Ok(dir.join(RELATIVE_SOCKET_PATH))
}
