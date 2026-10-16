//! The real `clipleaf-copy` binary against the test compositor: what ends up in the clipboard,
//! how long the process lives, and what the user sees when something is wrong. The paster is a
//! core session in the test thread. No sleeps, no timeouts: every wait is a compositor note, a
//! blocking read or a process wait. Spawned processes are killed on drop, also when an assertion
//! fails; detached children exit by themselves when the compositor closes their socket.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    os::unix::ffi::OsStrExt,
    process::{Command, Output, Stdio},
};

use clipleaf_core::{OFFERED_TXT_MIME_TYPES, WlBufferedStream, WlSessionEvent, WlSessionManager};
use clipleaf_test_common::{
    Config, Note, TestCompositor,
    bins::{self, Started},
    compositor, compositor_with, fds, procs,
};

const COPY: &str = env!("CARGO_BIN_EXE_clipleaf-copy");

fn paster(comp: &TestCompositor) -> WlSessionManager {
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    let mut session = WlSessionManager::initialize(stream).unwrap();
    let callback_id = session.schedule_sync().unwrap();
    while !matches!(session.next_event().unwrap(), WlSessionEvent::SyncDone{callback_id: done_callback_id} if done_callback_id == callback_id)
    {
    }
    session
}

/// What a paster gets for `mime` (or the preferred type) from the clipboard / primary selection.
fn pasted(comp: &TestCompositor, primary: bool, mime: Option<&str>) -> Vec<u8> {
    let mut session = paster(comp);
    let (mut reader, writer) = fds::channel();
    session
        .receive_offer(primary, mime, writer)
        .unwrap()
        .expect("type offered");
    session.flush().unwrap();
    // Blocking: the relay dups the fd it forwards, so EOF can be microseconds late.
    fds::read_to_eof(&mut reader)
}

/// Another program takes the clipboard (or primary), which cancels the current owner.
fn take_over(comp: &TestCompositor, primary: bool) -> WlSessionManager {
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    let mut session = WlSessionManager::initialize(stream).unwrap();
    let source = session.create_data_source().unwrap();
    source
        .offer(session.get_message_writer(), "text/plain")
        .unwrap();
    if primary {
        session.set_primary_selection(source.local_id).unwrap();
    } else {
        session.set_selection(source.local_id).unwrap();
    }
    let callback_id = session.schedule_sync().unwrap();
    while !matches!(session.next_event().unwrap(), WlSessionEvent::SyncDone{callback_id: done_callback_id} if done_callback_id == callback_id)
    {
    }
    session
}

/// Starts `clipleaf-copy --foreground ...` and waits until it owns the selection.
fn foreground(comp: &TestCompositor, args: &[&str], primary: bool) -> (Started, Vec<String>) {
    let mut all = vec!["--foreground"];
    all.extend(args);
    launch(comp, bins::command(COPY, comp, &all), None, primary)
}

/// Starts `command` and waits until it owns the selection. If it exits first, the test fails
/// with its stderr instead of hanging.
fn launch(
    comp: &TestCompositor,
    mut command: Command,
    input: Option<&[u8]>,
    primary: bool,
) -> (Started, Vec<String>) {
    command.stdout(Stdio::null());
    let mut started = bins::start(comp, command, input);
    let types = started.wait_selection(comp, primary);
    (started, types)
}

/// A nameless file holding `data`, positioned at the start, for use as a child's stdin.
fn stdin_file(data: &[u8]) -> File {
    let path = std::env::temp_dir().join(format!(
        "clipleaf-copy-test-{}-{}",
        std::process::id(),
        next_id()
    ));
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    fs::remove_file(&path).unwrap();
    file.write_all(data).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file
}

fn next_id() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Copies `stdin` (a pipe fed by the test, like `cmd | clipleaf-copy`) in the foreground.
fn copy_stdin(comp: &TestCompositor, args: &[&str], input: &[u8]) -> (Started, Vec<String>) {
    let mut all = vec!["--foreground"];
    all.extend(args);
    launch(comp, bins::command(COPY, comp, &all), Some(input), false)
}

fn assert_same(actual: &[u8], expected: &[u8], what: &str) {
    let first_diff = actual.iter().zip(expected).position(|(a, b)| a != b);
    assert!(
        actual == expected,
        "{what}: got {} bytes, expected {}, first difference at {first_diff:?}",
        actual.len(),
        expected.len()
    );
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---- what gets copied ----

#[test]
fn a_text_argument_is_offered_as_the_five_text_types_and_served_to_every_paster() {
    let comp = compositor();
    let (_copy, types) = foreground(&comp, &["hello"], false);
    assert_eq!(types, OFFERED_TXT_MIME_TYPES);
    assert_eq!(pasted(&comp, false, None), b"hello");
    assert_eq!(pasted(&comp, false, Some("UTF8_STRING")), b"hello");
    assert_eq!(pasted(&comp, false, None), b"hello");
}

#[test]
fn words_are_joined_with_single_spaces_and_options_may_follow_words() {
    for (args, expected) in [
        (&["a", "b c"][..], &b"a b c"[..]),
        (&["a", "--foreground", "b"], b"a b"),
        (&["--", "-p", "-f", "--type", "x"], b"-p -f --type x"),
        (&["-"], b"-"),
    ] {
        let comp = compositor();
        let (_copy, _) = foreground(&comp, args, false);
        assert_eq!(pasted(&comp, false, None), expected, "{args:?}");
    }
}

#[test]
fn a_word_that_is_not_utf8_is_copied_byte_for_byte() {
    let comp = compositor();
    let mut command = bins::command(COPY, &comp, &["--foreground"]);
    command.arg(OsStr::from_bytes(b"\xff\xfe")).arg("tail");
    let (_copy, _) = launch(&comp, command, None, false);
    assert_eq!(pasted(&comp, false, None), b"\xff\xfe tail");
}

#[test]
fn stdin_is_copied_whatever_its_size_and_served_again_to_every_paster() {
    // Up to 128 KiB stays in memory; above it, the input goes to a nameless temp file.
    for len in [0, 1, 128 << 10, (128 << 10) + 1, 3 << 20] {
        let comp = compositor();
        let expected = data(len);
        let (_copy, types) = copy_stdin(&comp, &[], &expected);
        assert_eq!(types, OFFERED_TXT_MIME_TYPES);
        for paste in 1..=2 {
            assert_same(
                &pasted(&comp, false, None),
                &expected,
                &format!("len {len}, paste {paste}"),
            );
        }
    }
}

#[test]
fn stdin_that_is_a_file_is_read_from_its_current_offset() {
    for len in [100, 3 << 20] {
        let comp = compositor();
        let expected = data(len);
        let mut input = stdin_file(&expected);
        input.seek(SeekFrom::Start(10)).unwrap();
        let mut command = bins::command(COPY, &comp, &["--foreground"]);
        command.stdin(input);
        let (_copy, _) = launch(&comp, command, None, false);
        assert_same(
            &pasted(&comp, false, None),
            &expected[10..],
            &format!("len {len}"),
        );
    }
}

#[test]
fn a_type_option_offers_only_that_type() {
    let libreoffice = r#"application/x-openoffice-objectdescriptor-xml;windows_formatname="Star Object Descriptor (XML)";displayname="file:///tmp/Résumé.docx""#;
    for mime in ["image/png", libreoffice] {
        let comp = compositor();
        let (_copy, types) = copy_stdin(&comp, &["-t", mime], b"\x89PNG");
        assert_eq!(types, [mime]);
        assert_eq!(pasted(&comp, false, Some(mime)), b"\x89PNG");
    }
}

#[test]
fn the_primary_option_sets_only_the_primary_selection() {
    let comp = compositor();
    let (_clip, _) = foreground(&comp, &["clip"], false);
    let (_prim, types) = foreground(&comp, &["-p", "prim"], true);
    assert_eq!(types, OFFERED_TXT_MIME_TYPES);
    assert_eq!(pasted(&comp, false, None), b"clip");
    assert_eq!(pasted(&comp, true, None), b"prim");
}

// ---- how long it lives ----

#[test]
fn in_the_foreground_it_serves_until_another_program_takes_the_clipboard() {
    let comp = compositor();
    let (copy, _) = foreground(&comp, &["once"], false);
    for _ in 0..3 {
        assert_eq!(pasted(&comp, false, None), b"once");
    }
    let _next = take_over(&comp, false);
    // Cancelled: exits successfully.
    assert!(copy.child.wait().success());
}

#[test]
fn by_default_the_parent_exits_after_the_sync_and_exactly_one_child_keeps_serving() {
    let comp = compositor();
    // `output()` returns only when the parent has exited AND every holder of its stdout/stderr
    // pipes has closed them, so this also proves the child detached from them.
    let output = bins::run(COPY, &comp, &["detached"]);
    assert!(output.status.success(), "{}", stderr(&output));
    // The set_selection was processed before the parent's sync returned, so its note is already
    // queued behind the connect. A parent that exited before syncing would race this and lose.
    let early: Vec<Note> = comp.notes.try_iter().collect();
    let Some(Note::ClientConnected(copy_client)) = early.first() else {
        panic!("expected the copy's connection first: {early:?}");
    };
    let copy_client = *copy_client;
    let selected = early.iter().find_map(|n| match n {
        Note::Selection {
            primary: false,
            mimes: Some(mimes),
        } => Some(mimes),
        _ => None,
    });
    assert_eq!(
        selected.expect("the selection was set before the parent exited"),
        &OFFERED_TXT_MIME_TYPES
    );
    assert_eq!(
        procs::find("clipleaf-copy", comp.runtime_dir()).len(),
        1,
        "the parent is gone and one child remains"
    );
    for _ in 0..2 {
        assert_eq!(pasted(&comp, false, None), b"detached");
    }

    // Replaced: the child is cancelled, exits, and its socket closes.
    let _next = take_over(&comp, false);
    comp.wait_for(|n| *n == Note::ClientDisconnected(copy_client));
}

// ---- temp files ----

#[test]
fn large_input_is_kept_in_the_given_temp_dir_without_leaving_a_name_behind() {
    let comp = compositor();
    let dir = comp.runtime_dir().join("spill");
    fs::create_dir(&dir).unwrap();
    let expected = data(300 << 10);
    let (_copy, _) = copy_stdin(&comp, &["--temp-dir", dir.to_str().unwrap()], &expected);
    assert_same(&pasted(&comp, false, None), &expected, "spilled input");
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "no named temp file");
}

#[test]
fn a_missing_temp_dir_matters_only_when_the_input_is_large() {
    let comp = compositor();
    let missing = comp.runtime_dir().join("missing");
    let missing = missing.to_str().unwrap();

    // Exactly 128 KiB stays in memory, so the missing directory is never touched.
    // (Detached, so that a failure shows up as the parent's exit status instead of a wait.)
    let output = bins::command(COPY, &comp, &["--temp-dir", missing])
        .stdin(stdin_file(&data(128 << 10)))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(pasted(&comp, false, None), data(128 << 10));

    // One byte more spills to a file in that directory, which fails.
    let output = bins::command(COPY, &comp, &["--temp-dir", missing])
        .stdin(stdin_file(&data((128 << 10) + 1)))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("--temp-dir") && err.contains("missing"),
        "{err}"
    );
}

// ---- how main reports what the argument parser rejects (the parser itself is unit tested) ----

#[test]
fn a_usage_error_is_exit_code_1_with_the_message_and_usage_on_stderr() {
    // No compositor is needed: the arguments are rejected before connecting.
    for (args, named) in [
        (&["-x", "text"][..], "'-x'"),
        (&["-t"], "'-t'"),
        (&["--temp-dir"], "'--temp-dir'"),
        (&["-t", "text/\u{1b}[31m", "x"], "'-t'"),
        (&["--file", "-t", "text/plain", "a.png"], "--file"),
    ] {
        let output = bins::bare(COPY, args).output().unwrap();
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
fn file_mode_says_it_is_not_implemented_yet() {
    let output = bins::bare(COPY, &["--file", "a.png"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("not implemented"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn help_goes_to_stdout_with_exit_code_0_even_without_a_compositor() {
    for args in [&["-h"][..], &["--help"], &["a", "-p", "-h"]] {
        let output = bins::bare(COPY, args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        let out = String::from_utf8(output.stdout).unwrap();
        assert!(
            out.contains("Usage:") && out.contains("--foreground"),
            "{out}"
        );
    }
}

// ---- connecting ----

#[test]
fn without_a_wayland_environment_it_says_what_is_missing() {
    let output = bins::bare(COPY, &["text"]).output().unwrap();
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
    let output = bins::bare(COPY, &["text"])
        .env("WAYLAND_DISPLAY", &missing)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("cannot connect to") && err.contains(missing.to_str().unwrap()),
        "{err}"
    );
}

#[test]
fn a_compositor_without_data_control_is_reported_by_protocol_name() {
    let comp = compositor_with(Config {
        data_control: false,
        ..Config::default()
    });
    let output = bins::run(COPY, &comp, &["text"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("ext_data_control_manager_v1"),
        "{}",
        stderr(&output)
    );
}
