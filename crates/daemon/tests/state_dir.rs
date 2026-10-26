//! The state dir as a whole: it is private to its user, a daemon killed in the middle of a copy
//! leaves nothing half-kept behind it, and only one daemon uses a history at a time.

mod common;

use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};
use clipleaf_test_common::{bins::Guarded, raw::RawCompositor};
use common::{wal::*, *};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// More than the 5 KiB inline limit, so the type gets a payload file.
fn big() -> Vec<u8> {
    vec![b'x'; 64 * 1024]
}

#[test]
fn the_history_is_private_to_its_user() {
    // Everything copied ends up here, passwords included: no one else may read or list it.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &big())]);
    let base = state.root().join("clipleaf");
    let file = state.payload_files().pop().expect("a payload file").0;
    assert_eq!(mode(&base), 0o700);
    assert_eq!(mode(&state.payloads()), 0o700);
    assert_eq!(mode(&state.wal()), 0o600);
    assert_eq!(mode(&base.join("lock")), 0o600);
    assert_eq!(mode(&state.payloads().join(file)), 0o600);
}

#[test]
fn a_daemon_killed_during_a_fetch_restarts_with_nothing_half_kept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let id = daemon.new_offer();
    daemon.send(&[offer(id, &[UTF8]), selection(id)]);
    let receive = daemon.receives_until(id).pop().unwrap();
    // Past the inline limit, so the daemon has a payload file open, and the source still open.
    let mut source = fs::File::from(receive.fd);
    source.write_all(&big()).unwrap();
    let start = Instant::now();
    while state.payload_files().is_empty() {
        assert!(start.elapsed() < WAIT, "no payload file for the fetch");
        thread::sleep(Duration::from_millis(1));
    }
    daemon.stop();
    drop(source);

    // The restart sweeps the partial file: there is no Insert for it to belong to.
    let mut daemon = Daemon::start_in(&state);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    assert!(records(&state.wal_bytes()).is_empty());
    let entry = daemon.copy_new(&[(UTF8, b"after the crash")]);
    assert_eq!(entry.sizes(), [(UTF8, 15)]);
}

#[test]
fn a_second_daemon_on_the_same_history_exits_with_an_error() {
    let state = StateDir::new();
    let mut first = Daemon::start_in(&state);
    first.copy_new(&[(UTF8, b"kept by the first")]);
    let wal = state.wal_bytes();

    // A compositor of its own, so a second daemon that got past the lock would connect.
    let comp = RawCompositor::new();
    let mut second = Guarded::new(
        Command::new(env!("CARGO_BIN_EXE_clipleafd"))
            .env_clear()
            .env("WAYLAND_DISPLAY", comp.path())
            .env("XDG_STATE_HOME", state.root())
            .env("CLIPLEAF_LOG", "off")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    let status = loop {
        if let Some(status) = second.child().try_wait().unwrap() {
            break status;
        }
        assert!(start.elapsed() < WAIT, "the second daemon kept running");
        thread::sleep(Duration::from_millis(1));
    };
    assert!(!status.success(), "{status}");
    let mut listener = [PollFd::new(comp.listener_fd(), PollEvents::IN)];
    assert!(matches!(
        poll(&mut listener, Some(Duration::ZERO)).unwrap(),
        PollResult::Timeout
    ));
    assert_eq!(state.wal_bytes(), wal);

    // The first one is untouched and keeps recording.
    first.copy_new(&[(UTF8, b"still recording")]);
}
