//! The real `clipleaf-paste` binary against the test compositor: what a user sees on stdout,
//! stderr and the exit code. The selection owner is a core session in the test thread, which
//! serves the paster after spawning it, so no thread, sleep or timeout is needed (every wait is a
//! blocking read or a process wait). Output goes to a file when it is larger than a pipe.

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Output, Stdio},
};

use clipleaf_core::{WlBufferedStream, WlSessionEvent, WlSessionManager, serve_bytes};
use clipleaf_test_common::{
    Config, Note, TestCompositor,
    bins::{self, Started},
    compositor, compositor_with,
};

const PASTE: &str = env!("CARGO_BIN_EXE_clipleaf-paste");

/// A selection owner: a core session that offers some types and can serve a paste.
struct Owner {
    session: WlSessionManager,
}

fn owner(comp: &TestCompositor, mimes: &[&str], primary: bool) -> Owner {
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    let mut session = WlSessionManager::initialize(stream).unwrap();
    let source = session.create_data_source().unwrap();
    for mime in mimes {
        source.offer(session.get_message_writer(), mime).unwrap();
    }
    if primary {
        session.set_primary_selection(source.local_id).unwrap();
    } else {
        session.set_selection(source.local_id).unwrap();
    }
    let callback_id = session.sync().unwrap();
    while !matches!(session.next_event().unwrap(), WlSessionEvent::SyncDone{callback_id: done_callback_id} if done_callback_id == callback_id)
    {
    }
    Owner { session }
}

impl Owner {
    /// Serves the next paste with `payload` and returns the type that was asked for. The paster
    /// must already be running (or about to run): this blocks until it asks.
    fn serve(&mut self, payload: &[u8]) -> String {
        loop {
            if let WlSessionEvent::DataControlSend { mime, fd, .. } =
                self.session.next_event().unwrap()
            {
                // The paster may stop reading early (broken pipe): not this side's problem.
                let _ = serve_bytes(fd, payload);
                return String::from_utf8(mime.into_vec()).unwrap();
            }
        }
    }
}

fn paste_command(comp: &TestCompositor, args: &[&str]) -> Command {
    bins::command(PASTE, comp, args)
}

/// Pastes with `args` while `owner` serves `payload`; returns the output and the type asked for.
fn paste(
    comp: &TestCompositor,
    owner: &mut Owner,
    args: &[&str],
    payload: &[u8],
) -> (Output, String) {
    let mut command = paste_command(comp, args);
    command.stdout(Stdio::piped());
    let mut started = bins::start(comp, command, None);
    asked_of(comp, &mut started);
    let asked = owner.serve(payload);
    let output = started.child.finish();
    (output, asked)
}

/// Waits until the paster's `receive` was forwarded to the owner. If the paster exits first,
/// the test fails with its stderr instead of waiting for a `send` that never comes.
fn asked_of(comp: &TestCompositor, started: &mut Started) {
    started.wait_for(comp, |n| matches!(n, Note::Forwarded { .. }));
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn pastes_the_best_type_of_the_clipboard_without_a_trailing_newline_off_a_terminal() {
    let comp = compositor();
    let mut owner = owner(&comp, &["text/html", "text/plain;charset=utf-8"], false);
    let (output, asked) = paste(&comp, &mut owner, &[], b"hello");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(output.stdout, b"hello");
    assert_eq!(asked, "text/plain;charset=utf-8");
}

#[test]
fn lists_the_types_in_offered_order() {
    let comp = compositor();
    let _owner = owner(&comp, &["text/html", "image/png", "UTF8_STRING"], false);
    for args in [&["-l"][..], &["--list-types"]] {
        let output = bins::run(PASTE, &comp, args);
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(output.stdout, b"text/html\nimage/png\nUTF8_STRING\n");
    }
}

#[test]
fn the_primary_selection_is_independent_of_the_clipboard() {
    let comp = compositor();
    let mut clip = owner(&comp, &["text/plain"], false);
    let mut prim = owner(&comp, &["text/html"], true);

    let (output, asked) = paste(&comp, &mut clip, &[], b"clip");
    assert_eq!(
        (output.stdout, asked.as_str()),
        (b"clip".to_vec(), "text/plain")
    );
    let (output, asked) = paste(&comp, &mut prim, &["--primary"], b"<b>prim</b>");
    assert_eq!(
        (output.stdout, asked.as_str()),
        (b"<b>prim</b>".to_vec(), "text/html")
    );
    let output = bins::run(PASTE, &comp, &["-l", "-p"]);
    assert_eq!(output.stdout, b"text/html\n");
    let output = bins::run(PASTE, &comp, &["-l"]);
    assert_eq!(output.stdout, b"text/plain\n");
}

#[test]
fn a_requested_type_is_asked_for_by_name() {
    let comp = compositor();
    let mut owner = owner(&comp, &["text/plain", "image/png"], false);
    for flag in ["-t", "--type"] {
        let (output, asked) = paste(&comp, &mut owner, &[flag, "image/png"], b"\x89PNG");
        assert!(output.status.success(), "{}", stderr(&output));
        assert_eq!(output.stdout, b"\x89PNG");
        assert_eq!(asked, "image/png");
    }
}

#[test]
fn a_type_that_is_not_offered_fails_and_lists_what_is() {
    let comp = compositor();
    let _owner = owner(&comp, &["text/plain", "image/png"], false);
    let output = bins::run(PASTE, &comp, &["-t", "image/jpeg"]);
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    let err = stderr(&output);
    assert!(
        err.contains("image/jpeg") && err.contains("not available"),
        "{err}"
    );
    assert!(
        err.contains("text/plain") && err.contains("image/png"),
        "{err}"
    );
    // Nothing was asked of the owner. The disconnect of the paster (client 2, after the owner)
    // is reported only after all of its requests were dispatched, so every `receive` it made
    // has produced its note by then.
    let mut forwarded = false;
    comp.wait_for(|n| {
        forwarded |= matches!(n, Note::Forwarded { .. });
        *n == Note::ClientDisconnected(2)
    });
    assert!(!forwarded, "a receive was forwarded");
}

#[test]
fn nothing_to_paste_is_an_error_for_paste_and_for_list() {
    let comp = compositor();
    let output = bins::run(PASTE, &comp, &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nothing found to paste"),
        "{}",
        stderr(&output)
    );
    let output = bins::run(PASTE, &comp, &["-l"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("No offered MIME types"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_owner_that_went_away_leaves_nothing_to_paste() {
    let comp = compositor();
    let owner = owner(&comp, &["text/plain"], false);
    drop(owner);
    comp.wait_cleared(false);
    let output = bins::run(PASTE, &comp, &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nothing found to paste"),
        "{}",
        stderr(&output)
    );
}

fn read_back(file: &mut File) -> Vec<u8> {
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut out = Vec::new();
    file.read_to_end(&mut out).unwrap();
    out
}

#[test]
fn a_big_binary_payload_arrives_byte_exact() {
    let comp = compositor();
    let mut owner = owner(&comp, &["application/octet-stream"], false);
    let payload: Vec<u8> = (0..(8u32 << 20))
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    // A file, not a pipe: this thread serves the paste and cannot also drain a stdout pipe.
    let mut out = tempfile();
    let mut command = paste_command(&comp, &["-t", "application/octet-stream"]);
    command.stdout(out.try_clone().unwrap());
    let mut started = bins::start(&comp, command, None);
    asked_of(&comp, &mut started);
    owner.serve(&payload);
    let output = started.child.finish();
    assert!(output.status.success(), "{}", stderr(&output));
    let got = read_back(&mut out);
    assert!(got == payload, "payload differs ({} bytes)", got.len());
}

#[test]
fn a_reader_that_stops_reading_ends_the_paste_quietly() {
    let comp = compositor();
    let mut owner = owner(&comp, &["application/octet-stream"], false);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader); // like `clipleaf-paste | head -c0`
    let mut command = paste_command(&comp, &["-t", "application/octet-stream"]);
    command.stdout(writer);
    let mut started = bins::start(&comp, command, None);
    asked_of(&comp, &mut started);
    owner.serve(&vec![7u8; 4 << 20]);
    let output = started.child.finish();
    assert!(output.status.success(), "{}", stderr(&output));
    // Debug builds log `debug:` lines, so "quiet" means no error line.
    assert!(!stderr(&output).contains("error:"), "{}", stderr(&output));
}

fn tempfile() -> File {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "clipleaf-paste-test-{}-{}",
        std::process::id(),
        next_id()
    ));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file
}

fn next_id() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ---- how main reports what the argument parser rejects (the parser itself is unit tested) ----

#[test]
fn a_usage_error_is_exit_code_1_with_the_message_and_usage_on_stderr() {
    // No compositor is needed: the arguments are rejected before connecting.
    for (args, named) in [
        (&["-x"][..], "'-x'"),
        (&["hello"], "'hello'"),
        (&["-t"], "'-t'"),
        (&["-t", "text/\u{1b}[31m"], "'-t'"),
    ] {
        let output = bins::bare(PASTE, args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(output.stdout, b"", "{args:?}");
        let err = stderr(&output);
        assert!(
            err.contains(named) && err.contains("Usage:"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn help_goes_to_stdout_with_exit_code_0_even_without_a_compositor() {
    for args in [&["-h"][..], &["--help"], &["-p", "-l", "-h"]] {
        let output = bins::bare(PASTE, args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        let out = String::from_utf8(output.stdout).unwrap();
        assert!(
            out.contains("Usage:") && out.contains("--no-newline"),
            "{out}"
        );
    }
}

// ---- connecting ----

#[test]
fn without_a_wayland_environment_it_says_what_is_missing() {
    let output = bins::bare(PASTE, &[]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("XDG_RUNTIME_DIR is not set"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_display_that_does_not_exist_names_the_socket_it_tried() {
    let comp = compositor();
    let missing = comp.runtime_dir().join("no-such-display");
    let output = bins::bare(PASTE, &[])
        .env("WAYLAND_DISPLAY", &missing)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("cannot connect to") && err.contains(Path::new(&missing).to_str().unwrap()),
        "{err}"
    );
}

#[test]
fn a_compositor_without_data_control_is_reported_by_protocol_name() {
    let comp = compositor_with(Config {
        data_control: false,
        ..Config::default()
    });
    let output = bins::run(PASTE, &comp, &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("ext_data_control_manager_v1"),
        "{}",
        stderr(&output)
    );
}

// ---- the terminal newline: appended only for text types, and not with -n ----

/// Pastes with a pseudo-terminal as stdout and returns what the terminal received.
fn paste_on_a_tty(args: &[&str], mimes: &[&str], payload: &[u8]) -> Vec<u8> {
    use clipleaf_test_common::rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_path = ptsname(&master, Vec::new()).unwrap();
    let slave = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(OsStr::from_bytes(slave_path.to_bytes()))
        .unwrap();

    let comp = compositor();
    let mut owner = owner(&comp, mimes, false);
    let mut command = paste_command(&comp, args);
    command.stdout(slave); // the Command owns the only copy; `start` drops it after spawning
    let mut started = bins::start(&comp, command, None);
    asked_of(&comp, &mut started);
    owner.serve(payload);
    let output = started.child.finish();
    assert!(output.status.success(), "{}", stderr(&output));

    // With every slave end closed, the master returns what is left and then EIO.
    let mut master = File::from(master);
    let mut got = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match master.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) if e.raw_os_error() == Some(5) => break, // EIO: the slave side is gone
            Err(e) => panic!("reading the terminal: {e}"),
        }
    }
    got
}

#[test]
fn on_a_terminal_text_gets_a_trailing_newline_but_not_with_n_and_not_other_types() {
    // The terminal turns the newline into CR LF.
    assert_eq!(paste_on_a_tty(&[], &["text/plain"], b"hello"), b"hello\r\n");
    assert_eq!(paste_on_a_tty(&["-n"], &["text/plain"], b"hello"), b"hello");
    assert_eq!(
        paste_on_a_tty(&["--no-newline"], &["text/plain"], b"hello"),
        b"hello"
    );
    assert_eq!(paste_on_a_tty(&[], &["image/png"], b"PNG"), b"PNG");
}
