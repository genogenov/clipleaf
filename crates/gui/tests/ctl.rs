//! `clipleaf-default-gui-ctl` against a fake GUI: the test binds gui.sock in a private runtime dir
//! and serves one connection. No test here reaches the "GUI not running → start it" path: the ctl
//! runs from a private dir without a GUI binary next to it (see `IsolatedCtl`).

mod common;

use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Output,
    thread::{self, JoinHandle},
};

use common::{IsolatedCtl, Runtime, TempDir, assert_no_panic, command, output};

/// What the fake GUI does after reading the command line.
enum Answer {
    Reply(&'static [u8]),
    Close,
}

/// A fake GUI: accepts ONE connection, reads up to `\n` (blocking), answers, and returns every byte
/// it received (for `Reply`, until the ctl closes).
struct FakeGui {
    path: PathBuf,
    handle: Option<JoinHandle<Vec<u8>>>,
}

impl FakeGui {
    fn start(rt: &Runtime, answer: Answer) -> Self {
        let path = rt.socket_path();
        let listener = UnixListener::bind(&path).expect("bind gui.sock");
        let handle = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut got = Vec::new();
            let mut buf = [0u8; 256];
            while !got.contains(&b'\n') {
                let n = s.read(&mut buf).expect("read");
                if n == 0 {
                    return got;
                }
                got.extend_from_slice(&buf[..n]);
            }
            match answer {
                Answer::Reply(line) => {
                    s.write_all(line).expect("write reply");
                    s.read_to_end(&mut got).expect("read to EOF");
                }
                Answer::Close => drop(s),
            }
            got
        });
        Self { path, handle: Some(handle) }
    }

    /// Call after the ctl exited. If the ctl never connected, a dummy connection (EOF at once)
    /// releases `accept`, so a broken ctl fails the test instead of hanging it.
    fn received(mut self) -> Vec<u8> {
        let _ = UnixStream::connect(&self.path);
        match self.handle.take().unwrap().join() {
            Ok(got) => got,
            Err(e) => std::panic::resume_unwind(e),
        }
    }
}

struct Setup {
    rt: Runtime,
    home: TempDir,
    ctl: IsolatedCtl,
}

impl Setup {
    fn new() -> Self {
        Self {
            rt: Runtime::new(),
            home: TempDir::new(),
            ctl: IsolatedCtl::new(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let out = output(&mut command(&self.ctl.bin, Some(&self.rt), self.home.path(), args));
        assert_no_panic(&out);
        out
    }
}

/// Runs the ctl with `args` against a fake GUI answering `answer`; returns (exit code, bytes received).
fn exchange(args: &[&str], answer: Answer) -> (Option<i32>, Vec<u8>) {
    let s = Setup::new();
    let gui = FakeGui::start(&s.rt, answer);
    let out = s.run(args);
    (out.status.code(), gui.received())
}

#[test]
fn show_sends_show_line_and_ok_exits_0() {
    assert_eq!(exchange(&["show"], Answer::Reply(b"ok\n")), (Some(0), b"show\n".to_vec()));
}

#[test]
fn no_argument_sends_toggle() {
    assert_eq!(exchange(&[], Answer::Reply(b"ok\n")), (Some(0), b"toggle\n".to_vec()));
}

#[test]
fn hide_sends_hide_line() {
    assert_eq!(exchange(&["hide"], Answer::Reply(b"ok\n")), (Some(0), b"hide\n".to_vec()));
}

#[test]
fn rejected_reply_exits_1() {
    assert_eq!(exchange(&["show"], Answer::Reply(b"rejected\n")), (Some(1), b"show\n".to_vec()));
}

#[test]
fn malformed_reply_exits_1() {
    assert_eq!(exchange(&["show"], Answer::Reply(b"okay\n")), (Some(1), b"show\n".to_vec()));
}

#[test]
fn gui_closing_without_reply_exits_1() {
    assert_eq!(exchange(&["toggle"], Answer::Close), (Some(1), b"toggle\n".to_vec()));
}

#[test]
fn help_prints_usage_and_exits_0() {
    let s = Setup::new();
    for arg in ["-h", "--help"] {
        let out = s.run(&[arg]);
        assert_eq!(out.status.code(), Some(0), "{arg}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Usage:") && stdout.contains("toggle"), "{arg}: {stdout}");
    }
}

#[test]
fn usage_errors_exit_2() {
    let s = Setup::new();
    for args in [&["bogus"][..], &["show", "hide"]] {
        let out = s.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

/// `hide` with no GUI running is a no-op success (it never starts the GUI).
#[test]
fn hide_without_gui_socket_exits_0() {
    let s = Setup::new();
    assert!(!s.rt.socket_path().exists());
    let out = s.run(&["hide"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
}

/// No XDG_RUNTIME_DIR: no socket path. Current behavior: an error on stderr, exit 1 (not "not
/// running", so not the `hide` no-op exit 0).
#[test]
fn hide_without_runtime_dir_is_an_error_not_a_panic() {
    let s = Setup::new();
    let out = output(&mut command(&s.ctl.bin, None, s.home.path(), &["hide"]));
    assert_no_panic(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("XDG_RUNTIME_DIR"), "{stderr}");
}
