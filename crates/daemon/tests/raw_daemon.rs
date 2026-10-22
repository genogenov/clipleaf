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
fn a_copy_is_fetched_as_its_best_text_and_image_and_kept_with_their_sizes() {
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
    assert_eq!(daemon.next_insert().sizes(), [(UTF8, 5), (PNG, 3)]);
}

#[test]
fn only_types_larger_than_the_inline_limit_get_a_file_and_it_holds_every_byte() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    // More than a pipe holds, so the fetch has to read while the source writes.
    let image: Vec<u8> = (0..300 * 1024).map(|i| (i % 251) as u8).collect();
    let entry = daemon.copy_new(&[(UTF8, b"hello"), (PNG, &image)]);

    let in_file: Vec<bool> = entry.types.iter().map(|t| t.in_file).collect();
    assert_eq!(in_file, [false, true]);
    assert_eq!(entry.types[0].head, b"hello");
    // The image is fetch position 1 of this entry.
    let name = format!("{}.1", entry.id);
    assert_eq!(state.payload_files(), [(name.clone(), image.len() as u64)]);
    assert_eq!(fs::read(state.payloads().join(name)).unwrap(), image);
}

#[test]
fn exactly_the_inline_limit_stays_inline_and_one_byte_more_gets_a_file() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let inline = daemon.copy_new(&[(UTF8, &vec![b'x'; 5 * 1024])]);
    assert!(!inline.types[0].in_file);
    assert_eq!(inline.types[0].head, vec![b'x'; 5 * 1024]);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );

    let file = daemon.copy_new(&[(UTF8, &vec![b'y'; 5 * 1024 + 1])]);
    assert!(file.types[0].in_file);
    let sizes: Vec<u64> = state.payload_files().iter().map(|(_, s)| *s).collect();
    assert_eq!(sizes, [5 * 1024 + 1]);
}

#[test]
fn empty_types_are_left_out_and_a_copy_with_only_empty_types_is_not_kept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let image = daemon.copy_new(&[(UTF8, b""), (PNG, b"png")]);
    assert_eq!(image.sizes(), [(PNG, 3)]);
    // Nothing fetched: no record. The next record is the next copy's (its job starts only
    // after the empty one has already seen EOF on its pipe).
    daemon.copy(&[(UTF8, b"")]);
    let next = daemon.copy_new(&[(UTF8, b"next")]);
    assert_eq!(next.types[0].head, b"next");
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&[(UTF8, b""), (PNG, b"png")]), image.id);
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
    assert_eq!(daemon.next_insert().types[0].head, b"b");
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
    let p = daemon.new_offer();
    daemon.send(&[
        offer(p, &[UTF8]),
        wire::msg(DEVICE, DEVICE_PRIMARY_SELECTION, &p.to_ne_bytes()),
    ]);
    // A copy that is fetched: a receive for P, sent first, would fail this copy's check that
    // every receive is its own.
    let x = daemon.new_offer();
    daemon.send(&[offer(x, &[UTF8]), selection(x)]);
    let receives = daemon.receives_until(x);
    let offers: Vec<u32> = receives.iter().map(|r| r.offer).collect();
    assert_eq!(offers, [x]);
    drop(receives);

    let a = daemon.new_offer();
    daemon.send(&[offer(a, &[UTF8, "x-kde-passwordManagerHint"]), selection(a)]);
    // X is destroyed while A's selection is read, so A is in a drain of its own: one drain
    // with A and C would fetch only C anyway.
    let receives = daemon.receives_until_destroyed(x);
    assert!(receives.iter().all(|r| r.offer != a));
    // A later copy that is fetched: any receive for A would have been sent before it.
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
