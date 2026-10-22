//! The real `clipleafd` binary against the scripted raw peer: which copies it fetches, which
//! types it asks for, what it keeps on disk, and that a source that never answers holds up
//! nothing but its own job.

mod common;

use std::fs;

use clipleaf_test_common::raw::{
    proto::*,
    wire::{self},
};
use common::*;

#[test]
fn a_copy_is_fetched_as_its_best_text_and_image_and_logged_with_their_sizes() {
    let mut daemon = Daemon::start();
    let a = daemon.new_offer();
    daemon.send(&[
        offer(a, &["text/html", UTF8, PNG, "application/x-foo"]),
        selection(a),
    ]);

    let receives = daemon.receives_until(a);
    let mimes: Vec<&str> = receives.iter().map(|r| r.mime.as_str()).collect();
    assert_eq!(mimes, [UTF8, PNG]);
    let sizes = [5, 3];
    for (receive, size) in receives.into_iter().zip(sizes) {
        answer(receive, &vec![b'x'; size]);
    }
    daemon.line_with(&format!("[({UTF8:?}, 5), (\"image/png\", 3)]"));
}

#[test]
fn only_types_larger_than_the_inline_limit_get_a_file_and_it_holds_every_byte() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    // More than a pipe holds, so the fetch has to read while the source writes.
    let image: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    daemon.copy_new(&[(UTF8, b"hello"), (PNG, &image)]);

    let files = state.payload_files();
    assert_eq!(files.len(), 1, "{files:?}");
    let (name, _) = &files[0];
    assert!(
        name.ends_with(".1"),
        "the image is fetch position 1: {name}"
    );
    assert_eq!(fs::read(state.payloads().join(name)).unwrap(), image);
}

#[test]
fn exactly_the_inline_limit_stays_inline_and_one_byte_more_gets_a_file() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &vec![b'x'; 5 * 1024])]);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );

    daemon.copy_new(&[(UTF8, &vec![b'y'; 5 * 1024 + 1])]);
    let sizes: Vec<u64> = state.payload_files().iter().map(|(_, s)| *s).collect();
    assert_eq!(sizes, [5 * 1024 + 1]);
}

#[test]
fn empty_types_are_left_out_and_a_copy_with_only_empty_types_is_not_kept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy(&[(UTF8, b""), (PNG, b"png")]);
    daemon.lines_with(&["[(\"image/png\", 3)]", APPENDED]);
    daemon.copy(&[(UTF8, b"")]);
    daemon.line_with(": [] in "); // the job's result line: nothing fetched
    daemon.stop();

    // Only the copy with something in it was kept.
    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
}

#[test]
fn a_source_that_never_answers_does_not_hold_up_the_next_copy() {
    let mut daemon = Daemon::start();
    let a = daemon.new_offer();
    daemon.send(&[offer(a, &[UTF8]), selection(a)]);
    let stalled = daemon.receives_until(a); // kept open, never written

    let b = daemon.new_offer();
    daemon.send(&[offer(b, &[UTF8]), selection(b)]);
    let mut receives = daemon.receives_until(b);
    assert_eq!(receives.len(), 1);
    answer(receives.remove(0), b"b");
    daemon.line_with(&format!("[({UTF8:?}, 1)]"));
    drop(stalled);
}

#[test]
fn a_burst_of_copies_in_one_read_fetches_only_the_last() {
    let mut daemon = Daemon::start();
    let (a, b, c) = (daemon.new_offer(), daemon.new_offer(), daemon.new_offer());
    daemon.send(&[
        offer(a, &[UTF8]),
        selection(a),
        offer(b, &[UTF8]),
        selection(b),
        offer(c, &[UTF8]),
        selection(c),
    ]);
    let offers: Vec<u32> = daemon.receives_until(c).iter().map(|r| r.offer).collect();
    assert_eq!(offers, [c]);
}

#[test]
fn password_manager_copies_and_the_primary_selection_are_not_fetched() {
    let mut daemon = Daemon::start();
    let a = daemon.new_offer();
    daemon.send(&[offer(a, &[UTF8, "x-kde-passwordManagerHint"]), selection(a)]);
    // Each change is logged before its fetch would start, so waiting for the line keeps the
    // three copies in separate drains (one drain would fetch only the last one anyway).
    daemon.line_with("Offered mime types: text/plain;charset=utf-8, x-kde-passwordManagerHint");
    let b = daemon.new_offer();
    daemon.send(&[
        offer(b, &[UTF8]),
        wire::msg(DEVICE, DEVICE_PRIMARY_SELECTION, &b.to_ne_bytes()),
    ]);
    daemon.line_with("Offered mime types: text/plain;charset=utf-8");
    // A later copy that is fetched: any receive for A or B would have been sent before it.
    let c = daemon.new_offer();
    daemon.send(&[offer(c, &[UTF8]), selection(c)]);
    let offers: Vec<u32> = daemon.receives_until(c).iter().map(|r| r.offer).collect();
    assert_eq!(offers, [c]);
}

#[test]
fn the_daemon_exits_with_an_error_when_the_compositor_goes_away() {
    let mut daemon = Daemon::start();
    // One fetched copy first, so the hang-up comes while the loop is running, not during setup.
    let a = daemon.new_offer();
    daemon.send(&[offer(a, &[UTF8]), selection(a)]);
    let _pending = daemon.receives_until(a);

    let status = daemon.hang_up();
    assert_eq!(status.code(), Some(1), "{status}");
}
