//! Restore: when the clipboard goes empty (its owner left or cleared it), or is empty when the
//! daemon starts, the daemon takes the newest entry back to the clipboard with a source of its
//! own. That source offers every stored type (a text type also under the other standard text
//! names) and a private marker, so the daemon never fetches its own selection. Pastes are served
//! by a pool: an unknown type gets EOF, a paste in flight finishes even after the source is
//! cancelled, a paster that stops reading is dropped after the 5 s stall, and pastes still
//! queued when the selection changes are dropped.

mod common;

use std::{
    thread,
    time::{Duration, Instant},
};

use common::*;

const TEXT_NAMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
const URI_LIST: &str = "text/uri-list";
const KDE_CUT: &str = "application/x-kde-cutselection";
const GNOME: &str = "x-special/gnome-copied-files";
/// The daemon's serve stall (offer/serve.rs).
const STALL: Duration = Duration::from_secs(5);

/// Over the 5 KiB inline limit, so it is served from a payload file.
fn big(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| b'a' + ((i + seed as usize) % 26) as u8)
        .collect()
}

fn marker_of(source: &Source) -> &str {
    source
        .mimes
        .iter()
        .find(|m| m.starts_with("x-clipleaf/"))
        .expect("a marker")
}

#[test]
fn a_cleared_clipboard_gets_the_newest_entry_back_with_every_stored_type() {
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(UTF8, b"older")]);
    let image = big(64 * 1024, 1);
    daemon.copy_new(&[(UTF8, b"newest text"), (PNG, &image)]);

    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();

    // Stored order, the text type under every standard text name, the marker last.
    let mut expected: Vec<&str> = TEXT_NAMES.to_vec();
    expected.push(PNG);
    assert_eq!(source.mimes[..6], expected[..], "{:?}", source.mimes);
    assert_eq!(source.mimes.len(), 7, "{:?}", source.mimes);
    let marker = &source.mimes[6];
    let nonce = marker.strip_prefix("x-clipleaf/").expect("the marker");
    assert!(
        nonce.len() == 8 && nonce.bytes().all(|b| b.is_ascii_hexdigit()),
        "{marker}"
    );

    // Every text name gets the text, byte-exact; the image comes from its payload file.
    for name in TEXT_NAMES {
        assert_eq!(
            daemon.paste(source.id, name.as_bytes()),
            b"newest text",
            "{name}"
        );
    }
    assert!(
        daemon.paste(source.id, PNG.as_bytes()) == image,
        "image bytes differ"
    );
}

#[test]
fn the_daemons_own_selection_is_not_fetched_and_the_next_copy_is() {
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(UTF8, b"kept")]);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();

    // What the compositor sends next: our own offer as the new selection. `copy` panics on a
    // receive for any offer but its own, so a fetch of ours would fail here.
    let own = daemon.new_offer();
    let mimes: Vec<&str> = source.mimes.iter().map(String::as_str).collect();
    daemon.send(&[offer(own, &mimes), selection(own)]);
    // A round trip through the daemon's loop before the next copy, so that copy lands in a later
    // read: a fetch of our own offer would already be on the wire by then. (The paste itself may
    // come back empty: the own selection's change drops a paste queued in the same read.)
    read_to_eof(daemon.paste_start(source.id, UTF8.as_bytes()));
    let next = daemon.copy_new(&[(UTF8, b"a real copy")]);
    assert_eq!(next.sizes(), [(UTF8, 11)]);
}

#[test]
fn an_empty_clipboard_at_startup_gets_the_newest_entry_and_an_empty_history_gets_nothing() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, b"first")]);
    daemon.copy_new(&[(UTF8, b"second")]);
    daemon.stop();

    // The bind-time selection event of a compositor whose clipboard is empty.
    let mut daemon = Daemon::start_in(&state);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();
    assert_eq!(daemon.paste(source.id, UTF8.as_bytes()), b"second");

    // A fresh history has nothing to restore: the next copy is simply recorded.
    let mut empty = Daemon::start();
    empty.send(&[cleared()]);
    empty.copy_new(&[(UTF8, b"after nothing")]);
    assert!(empty.sets().is_empty(), "{:?}", empty.sets());
}

#[test]
fn a_type_that_was_not_offered_gets_eof_and_the_next_paste_still_works() {
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(UTF8, b"only text")]);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();
    let marker = marker_of(&source).to_owned();

    assert_eq!(daemon.paste(source.id, PNG.as_bytes()), b"");
    assert_eq!(daemon.paste(source.id, marker.as_bytes()), b"");
    assert_eq!(daemon.paste(source.id, b"\xff\xfe not utf-8"), b"");
    assert_eq!(daemon.paste(source.id, UTF8.as_bytes()), b"only text");
}

#[test]
fn a_paste_in_flight_finishes_after_the_source_is_cancelled() {
    // 8 MiB through a 1 MiB pipe: the serve is still running when another app takes the
    // clipboard. The source is destroyed, the paste completes byte-exact.
    let mut daemon = Daemon::start();
    let text = big(8 << 20, 2);
    daemon.copy_new(&[(UTF8, &text)]);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();

    let mut pipe = daemon.paste_start(source.id, UTF8.as_bytes());
    let mut first = vec![0u8; 64 * 1024];
    std::io::Read::read_exact(&mut pipe, &mut first).unwrap();
    daemon.cancel(source.id);
    daemon.wait_source_destroyed(source.id);
    let mut got = first;
    got.extend(read_to_eof(pipe));
    assert!(
        got == text,
        "{} of {} bytes, or different bytes",
        got.len(),
        text.len()
    );
}

#[test]
fn the_previous_source_is_destroyed_when_the_daemon_restores_again() {
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(UTF8, b"again")]);
    daemon.send(&[cleared()]);
    let first = daemon.wait_restore();
    daemon.send(&[cleared()]);
    let second = daemon.wait_restore();
    daemon.wait_source_destroyed(first.id);
    assert!(!daemon.source(second.id).destroyed);
    assert_eq!(daemon.paste(second.id, UTF8.as_bytes()), b"again");
}

#[test]
fn a_cut_is_offered_as_it_was_stored_marker_and_all() {
    // After the first paste the cut paths are stale, so a second paste fails harmlessly; one that
    // was never pasted still moves the files, as the user intended.
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(URI_LIST, b"file:///home/u/a.txt\r\n"), (KDE_CUT, b"1")]);
    daemon.send(&[cleared()]);
    let kde = daemon.wait_restore();
    assert!(kde.mimes.iter().any(|m| m == KDE_CUT), "{:?}", kde.mimes);
    assert_eq!(daemon.paste(kde.id, KDE_CUT.as_bytes()), b"1");
    assert_eq!(
        daemon.paste(kde.id, URI_LIST.as_bytes()),
        b"file:///home/u/a.txt\r\n"
    );

    let gnome_cut = b"cut\nfile:///home/u/b.txt";
    daemon.copy_new(&[(GNOME, gnome_cut)]);
    daemon.send(&[cleared()]);
    let gnome = daemon.wait_restore();
    assert_eq!(daemon.paste(gnome.id, GNOME.as_bytes()), gnome_cut);
}

#[test]
fn a_clear_followed_by_a_new_copy_in_one_read_is_just_the_new_copy() {
    // The newer copy is already in the daemon's buffer: restoring the old entry for the clear
    // would put it over the user's new copy.
    let mut daemon = Daemon::start();
    daemon.copy_new(&[(UTF8, b"old entry")]);
    let id = daemon.new_offer();
    daemon.send(&[cleared(), offer(id, &[UTF8]), selection(id)]);
    let receives = daemon.receives_until(id);
    for receive in receives {
        answer(receive, b"new copy");
    }
    assert_eq!(daemon.next_insert().sizes(), [(UTF8, 8)]);
    assert!(daemon.sets().is_empty(), "{:?}", daemon.sets());
}

#[test]
fn a_paster_that_stops_reading_is_dropped_after_the_stall_and_others_are_served_meanwhile() {
    // 4 MiB fills the paster's 1 MiB pipe, so the serve waits on it from the start.
    let mut daemon = Daemon::start();
    let text = big(4 << 20, 3);
    daemon.copy_new(&[(UTF8, &text)]);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();

    let stalled = daemon.paste_start(source.id, UTF8.as_bytes());
    let started = Instant::now();
    // Another worker serves this one whole while the first waits.
    assert!(
        daemon.paste(source.id, UTF8.as_bytes()) == text,
        "the second paste differs"
    );
    assert!(started.elapsed() < STALL, "{:?}", started.elapsed());

    // After the stall the daemon gives up: what is left in the pipe, then EOF.
    thread::sleep(STALL + Duration::from_secs(1) - started.elapsed());
    let got = read_to_eof(stalled);
    assert!(
        got.len() < text.len(),
        "the stalled paste got all {} bytes",
        got.len()
    );
    assert!(text.starts_with(&got));
}

#[test]
fn pastes_still_queued_when_the_selection_changes_are_dropped() {
    // Four pasters that never read hold all four workers; the fifth paste waits in the queue.
    // A new copy drops it (EOF at once), while the four stay open until their stall.
    let mut daemon = Daemon::start();
    let text = big(4 << 20, 4);
    daemon.copy_new(&[(UTF8, &text)]);
    daemon.send(&[cleared()]);
    let source = daemon.wait_restore();

    let busy: Vec<_> = (0..4)
        .map(|_| daemon.paste_start(source.id, UTF8.as_bytes()))
        .collect();
    let queued = daemon.paste_start(source.id, UTF8.as_bytes());
    let started = Instant::now();
    daemon.copy_new(&[(UTF8, b"a new copy")]);
    assert_eq!(read_to_eof(queued), b"");
    assert!(
        started.elapsed() < STALL,
        "dropped only at the stall: {:?}",
        started.elapsed()
    );
    drop(busy);
}
