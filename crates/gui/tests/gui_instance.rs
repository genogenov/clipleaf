//! `clipleaf-default-gui` paths that exit before GTK starts: usage, and the losing side of the
//! single-instance lock. Nothing here may reach `build_app`.

mod common;

use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::Path,
};

use common::{GUI, Runtime, TempDir, assert_no_panic, command, output};

#[test]
fn help_prints_usage_and_exits_0() {
    let (rt, home) = (Runtime::new(), TempDir::new());
    let out = output(&mut command(Path::new(GUI), Some(&rt), home.path(), &["--help"]));
    assert_no_panic(&out);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage:"));
    assert!(!rt.path().join("clipleaf-default-gui").exists(), "--help must not touch the runtime dir");
}

#[test]
fn unknown_argument_exits_2() {
    let (rt, home) = (Runtime::new(), TempDir::new());
    let out = output(&mut command(Path::new(GUI), Some(&rt), home.path(), &["--bogus"]));
    assert_no_panic(&out);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!rt.path().join("clipleaf-default-gui").exists());
}

/// Another instance holds the lock: exit 0 at once, and leave the winner's gui.sock alone
/// (the lock is taken before the stale socket is removed).
#[test]
fn second_instance_exits_0_and_keeps_the_winners_socket() {
    let (rt, home) = (Runtime::new(), TempDir::new());
    let dir = rt.gui_dir();
    let lock = File::create(dir.join("lock")).unwrap();
    lock.lock().expect("flock");
    let sock = dir.join("gui.sock");
    fs::write(&sock, b"winner").unwrap();
    let ino = fs::metadata(&sock).unwrap().ino();

    let out = output(&mut command(Path::new(GUI), Some(&rt), home.path(), &[]));
    assert_no_panic(&out);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read(&sock).expect("winner's gui.sock removed"), b"winner");
    assert_eq!(fs::metadata(&sock).unwrap().ino(), ino);
    drop(lock);
}
