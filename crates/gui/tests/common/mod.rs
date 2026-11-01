//! Shared helpers for the clipleaf-default-gui integration tests.
//!
//! Every spawned binary gets a cleared environment plus a private 0700 `XDG_RUNTIME_DIR` and
//! `HOME`, so nothing can reach the user's Wayland session or a running GUI.

#![allow(dead_code)]

use std::{
    fs::{self, DirBuilder},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU32, Ordering},
};

pub const CTL: &str = env!("CARGO_BIN_EXE_clipleaf-default-gui-ctl");
pub const GUI: &str = env!("CARGO_BIN_EXE_clipleaf-default-gui");

/// A private dir removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Under `parent`, mode 0700.
    pub fn new_in(parent: &Path, prefix: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{prefix}-{}-{n}", std::process::id()));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    /// Under `temp_dir()`: short enough for a socket path (SUN_LEN).
    pub fn new() -> Self {
        Self::new_in(&std::env::temp_dir(), "cl-gui")
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A private runtime dir plus the GUI's socket dir inside it.
pub struct Runtime {
    pub dir: TempDir,
}

impl Runtime {
    pub fn new() -> Self {
        Self { dir: TempDir::new() }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `<runtime>/clipleaf-default-gui`, created 0700.
    pub fn gui_dir(&self) -> PathBuf {
        let dir = self.path().join("clipleaf-default-gui");
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => panic!("create {}: {e}", dir.display()),
        }
        dir
    }

    pub fn socket_path(&self) -> PathBuf {
        self.gui_dir().join("gui.sock")
    }
}

/// `bin` with an empty environment except PATH, HOME and XDG_RUNTIME_DIR (both private), stdin closed.
/// `runtime: None` leaves XDG_RUNTIME_DIR unset.
pub fn command(bin: &Path, runtime: Option<&Runtime>, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.args(args).env_clear().stdin(Stdio::null()).env("HOME", home);
    if let Some(path) = std::env::var_os("PATH") {
        c.env("PATH", path);
    }
    if let Some(rt) = runtime {
        c.env("XDG_RUNTIME_DIR", rt.path());
    }
    c
}

pub fn output(c: &mut Command) -> Output {
    c.output().expect("spawn")
}

/// The ctl hard-linked alone into a private dir. The ctl starts the GUI from its own dir
/// (`current_exe()` sibling), so a bug that reaches the spawn path fails with "not found" instead of
/// starting a GTK process. A hard link (same filesystem as the target dir) has no write fd that a
/// concurrently forked child could inherit (ETXTBSY).
pub struct IsolatedCtl {
    dir: TempDir,
    pub bin: PathBuf,
}

impl IsolatedCtl {
    pub fn new() -> Self {
        let dir = TempDir::new_in(Path::new(env!("CARGO_TARGET_TMPDIR")), "ctl");
        let bin = dir.path().join("clipleaf-default-gui-ctl");
        fs::hard_link(CTL, &bin).expect("hard link the ctl");
        assert!(!dir.path().join("clipleaf-default-gui").exists());
        Self { dir, bin }
    }
}

pub fn assert_no_panic(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "panic: {stderr}");
}
