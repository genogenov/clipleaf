//! The history survives a restart: what `clipleafd` writes to `wal.log` and `payloads/` is
//! read back by the next daemon on the same state dir, and damage is cut, skipped or reset as
//! decided (decisions.md 2026-10-20/21) without a panic. Each "restart" is a SIGKILL, so the
//! next daemon sees exactly what a crash leaves. What a daemon restored is asked by copying
//! (`Daemon::has`, `copy_dup`), so the questions about missing entries come last.

mod common;

use std::fs::{self, OpenOptions};

use common::{wal::*, *};

fn append(state: &StateDir, bytes: &[u8]) {
    use std::io::Write;
    OpenOptions::new()
        .append(true)
        .open(state.wal())
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn set_wal_len(state: &StateDir, len: u64) {
    OpenOptions::new()
        .write(true)
        .open(state.wal())
        .unwrap()
        .set_len(len)
        .unwrap();
}

/// Breaks a copy of the log in place.
type Damage = fn(&mut Vec<u8>);

/// More than the 5 KiB inline limit, so the type is kept in a payload file.
fn big(seed: u8) -> Vec<u8> {
    (0..6000u32).map(|i| (i % 251) as u8 ^ seed).collect()
}

/// The text of a copy of one inline text type.
fn text(t: &[u8]) -> [(&str, &[u8]); 1] {
    [(UTF8, t)]
}

#[test]
fn the_history_survives_a_restart_and_a_recopy_after_it_is_a_duplicate() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    // Two types in one entry: inline text and an image in a payload file.
    let big0 = big(0);
    let a_types = [(UTF8, &b"hello"[..]), (PNG, &big0)];
    let a = daemon.copy_new(&a_types);
    let b = daemon.copy_new(&text(b"world"));
    let files = state.payload_files();
    assert_eq!(files, [(format!("{}.1", a.id), 6000)]);
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(
        state.payload_files(),
        files,
        "a restored entry keeps its file"
    );
    // The same copy again matches the restored entry: its types, sizes and hashes came back.
    assert_eq!(daemon.copy_dup(&a_types), a.id);
    assert_eq!(daemon.copy_dup(&text(b"world")), b.id);
    // A new copy with the same layout gets an id after every old one: its file can't collide.
    let c = daemon.copy_new(&[(UTF8, b"hello"), (PNG, &big(1))]);
    assert!(c.id > b.id, "{} after {}", c.id, b.id);
    assert_eq!(state.payload_files().len(), 2);
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.payload_files().len(), 2);
    assert_eq!(daemon.copy_dup(&a_types), a.id);
    assert_eq!(daemon.copy_dup(&text(b"world")), b.id);
    assert_eq!(daemon.copy_dup(&[(UTF8, b"hello"), (PNG, &big(1))]), c.id);
}

#[test]
fn a_recopied_entry_stays_newer_than_the_ones_before_it_after_a_restart() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let a = daemon.copy_new(&text(&big(1)));
    let b = daemon.copy_new(&text(&big(2)));
    assert_eq!(daemon.copy_dup(&text(&big(1))), a.id); // A again: now newer than B
    daemon.stop();

    // 99 more copies push the oldest out of the 100 kept.
    let mut daemon = Daemon::start_in(&state);
    for i in 0..99 {
        daemon.copy_new(&text(format!("entry {i}").as_bytes()));
    }
    assert_eq!(daemon.next_delete(), b.id);
    assert_eq!(state.payload_files(), [(format!("{}.0", a.id), 6000)]);
}

#[test]
fn a_touch_of_an_unknown_entry_is_ignored_but_its_seq_is_never_reused() {
    // A Touch can outlive its entry only in a hand-made log, but its seq was handed out once.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let first = daemon.copy_new(&text(b"first"));
    daemon.stop();
    let body = [999u64, 1000, 0].map(u64::to_le_bytes).concat();
    append(&state, &record(salt_of(&state.wal_bytes()), TOUCH, &body));
    let with_touch = state.wal_len();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len(), with_touch, "a valid record is not cut");
    let next = daemon.copy_new(&text(b"second"));
    assert!(next.id > 1000, "{}", next.id);
    assert_eq!(daemon.copy_dup(&text(b"first")), first.id);
}

#[test]
fn an_evicted_entry_stays_gone_after_a_restart() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let oldest = daemon.copy_new(&text(&big(0))); // with a file
    assert_eq!(state.payload_files().len(), 1);
    for i in 0..100 {
        daemon.copy_new(&text(format!("entry {i}").as_bytes()));
    }
    // The 101st entry pushed the oldest out of the 100 kept.
    assert_eq!(daemon.next_delete(), oldest.id);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert!(state.payload_files().is_empty());
    assert!(daemon.has(&text(b"entry 0")));
    assert!(daemon.has(&text(b"entry 99")));
    assert!(!daemon.has(&text(&big(0))));
}

#[test]
fn a_torn_or_zero_filled_tail_is_cut_and_the_history_before_it_kept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let first = daemon.copy_new(&text(b"first"));
    let after_first = state.wal_len();
    daemon.copy_new(&text(&big(0)));
    daemon.stop();

    // A crash in the middle of writing the last record: its file goes with it.
    set_wal_len(&state, state.wal_len() - 3);
    let daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len(), after_first);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    daemon.stop();

    // A power cut can leave the file longer than what was written, the rest zeros.
    append(&state, &[0; 4096]);
    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len(), after_first);
    assert_eq!(daemon.copy_dup(&text(b"first")), first.id);
    assert!(!daemon.has(&text(&big(0))));
}

#[test]
fn a_damaged_record_is_dropped_with_everything_after_it() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let first = daemon.copy_new(&text(b"first"));
    let after_first = state.wal_len();
    daemon.copy_new(&text(b"second"));
    daemon.copy_new(&text(b"third"));
    daemon.stop();

    let mut wal = state.wal_bytes();
    wal[after_first as usize + 6] ^= 0x40; // inside the second record's body
    fs::write(state.wal(), &wal).unwrap();
    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len(), after_first);
    assert_eq!(daemon.copy_dup(&text(b"first")), first.id);
    assert!(!daemon.has(&text(b"second")));
    assert!(!daemon.has(&text(b"third")));
}

#[test]
fn a_record_of_an_unknown_type_is_skipped_and_the_records_after_it_kept() {
    // What a newer build may write: an older build must not cut the log there.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let first = daemon.copy_new(&text(b"first"));
    daemon.stop();
    append(
        &state,
        &record(salt_of(&state.wal_bytes()), 99, b"from the future"),
    );
    let with_unknown = state.wal_len();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len(), with_unknown, "the unknown record stays");
    let second = daemon.copy_new(&text(b"second"));
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&text(b"first")), first.id);
    assert_eq!(daemon.copy_dup(&text(b"second")), second.id);
}

#[test]
fn an_unusable_header_starts_an_empty_history_and_clears_the_payloads() {
    let cases: [(&str, Damage); 4] = [
        ("not our identifier", |w| w[0] ^= 0xff),
        ("a newer version", |w| {
            let h = header(2, salt_of(w));
            w[..HEADER_LEN].copy_from_slice(&h);
        }),
        ("a bad checksum", |w| w[HEADER_LEN - 1] ^= 1),
        ("shorter than a header", |w| w.truncate(20)),
    ];
    for (what, damage) in cases {
        let state = StateDir::new();
        let mut daemon = Daemon::start_in(&state);
        daemon.copy_new(&text(&big(0)));
        daemon.stop();
        let mut wal = state.wal_bytes();
        let old_salt = salt_of(&wal);
        damage(&mut wal);
        fs::write(state.wal(), &wal).unwrap();

        let mut daemon = Daemon::start_in(&state);
        assert!(state.payload_files().is_empty(), "{what}");
        let fresh = state.wal_bytes();
        assert_eq!(fresh.len(), HEADER_LEN, "{what}");
        assert_eq!(&fresh[..14], IDENTIFIER, "{what}");
        assert_ne!(
            salt_of(&fresh),
            old_salt,
            "{what}: a fresh header has a new salt"
        );
        // The fresh log works: what's copied now is there after the next restart.
        let after = daemon.copy_new(&text(b"after"));
        daemon.stop();
        let mut daemon = Daemon::start_in(&state);
        assert_eq!(daemon.copy_dup(&text(b"after")), after.id, "{what}");
        assert!(!daemon.has(&text(&big(0))), "{what}");
    }
}

#[test]
fn an_impossible_sequence_number_starts_an_empty_history() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&text(&big(0)));
    daemon.stop();
    // A valid Insert (id 5, seq u64::MAX, time 0, not pinned, no types): the next seq can't exist.
    let body = [
        &5u64.to_le_bytes()[..],
        &u64::MAX.to_le_bytes(),
        &0u64.to_le_bytes(),
        &[0],
        &0u32.to_le_bytes(),
    ]
    .concat();
    append(&state, &record(salt_of(&state.wal_bytes()), INSERT, &body));

    let mut daemon = Daemon::start_in(&state);
    assert!(state.payload_files().is_empty());
    assert_eq!(state.wal_len(), HEADER_LEN as u64);
    // Records written after the reset use its new salt: they survive the next restart.
    let after = daemon.copy_new(&text(b"after"));
    daemon.stop();
    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&text(b"after")), after.id);
    assert!(!daemon.has(&text(&big(0))));
}

#[test]
fn an_entry_with_a_missing_or_short_payload_is_dropped_whole_and_strays_are_swept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let (big1, big2, big3) = (big(1), big(2), big(3));
    daemon.copy_new(&text(&big1)); // A: one file
    daemon.copy_new(&[(UTF8, &big2), (PNG, &big3)]); // B: two files
    let c = daemon.copy_new(&text(b"inline")); // C: none
    daemon.stop();

    let files = state.payload_files();
    assert_eq!(files.len(), 3, "{files:?}");
    let path = |i: usize| state.payloads().join(&files[i].0);
    fs::remove_file(path(0)).unwrap(); // A's file is gone
    // B's first file is fine, its second one short: the whole entry goes, both files with it.
    OpenOptions::new()
        .write(true)
        .open(path(2))
        .unwrap()
        .set_len(files[2].1 - 1)
        .unwrap();
    fs::write(state.payloads().join("999.0"), b"left by a crash").unwrap();
    fs::create_dir(state.payloads().join("1000.0")).unwrap();
    fs::write(state.payloads().join("1000.0/inside"), b"x").unwrap();

    let mut daemon = Daemon::start_in(&state);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    assert_eq!(daemon.copy_dup(&text(b"inline")), c.id);
    assert!(!daemon.has(&text(&big1)));
    assert!(!daemon.has(&[(UTF8, &big2), (PNG, &big3)]));
}
