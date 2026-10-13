//! Finding (and, as a backstop, killing) processes that belong to one test.
//!
//! A process belongs to a test only if its `comm` is exactly the given name AND its environment
//! has that test's exact `XDG_RUNTIME_DIR`. Never match on command-line patterns.

use std::{ffi::OsStr, fs, os::unix::ffi::OsStrExt, path::Path};

use rustix::process::{Pid, Signal, kill_process};

/// Names of the binaries under test.
pub const BINARIES: [&str; 2] = ["clipleaf-copy", "clipleaf-paste"];

/// Pids of live processes named exactly `name` whose environment sets `XDG_RUNTIME_DIR=<runtime_dir>`.
pub fn find(name: &str, runtime_dir: &Path) -> Vec<u32> {
    let mut wanted = b"XDG_RUNTIME_DIR=".to_vec();
    wanted.extend_from_slice(runtime_dir.as_os_str().as_bytes());

    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid) = OsStr::to_str(&file_name).and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(comm) = fs::read(entry.path().join("comm")) else {
            continue; // exited meanwhile
        };
        if comm.strip_suffix(b"\n").unwrap_or(&comm) != name.as_bytes() {
            continue;
        }
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ.split(|b| *b == 0).any(|var| var == wanted) {
            found.push(pid);
        }
    }
    found
}

/// SIGKILLs any of our binaries still running for this runtime dir. A backstop for a test that
/// failed halfway: well-behaved runs leave nothing to kill.
pub fn kill_leftovers(runtime_dir: &Path) {
    for name in BINARIES {
        for pid in find(name, runtime_dir) {
            if let Some(pid) = Pid::from_raw(pid as i32) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}
