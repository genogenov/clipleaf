//! The history survives a restart: what `clipleafd` writes to `wal.log` and `payloads/` is
//! read back by the next daemon on the same state dir, and damage is cut, skipped or reset as
//! decided (decisions.md 2026-10-20/21) without a panic. Each "restart" is a SIGKILL, so the
//! next daemon sees exactly what a crash leaves.

mod common;

use std::fs::{self, OpenOptions};

use common::*;
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

/// The on-disk header: identifier, version u16, salt u64, seq_snapshot u64, checksum u64.
const IDENTIFIER: &[u8; 14] = b"CLIPLEAF WAL\0\0";
const HEADER_LEN: usize = 40;
const INSERT: u8 = 1;
const TOUCH: u8 = 3;

fn salt_of(wal: &[u8]) -> u64 {
    u64::from_le_bytes(wal[16..24].try_into().unwrap())
}

fn header(version: u16, salt: u64) -> Vec<u8> {
    let mut h = IDENTIFIER.to_vec();
    h.extend(version.to_le_bytes());
    h.extend(salt.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(xxh3_64(&h).to_le_bytes());
    h
}

/// A record with a valid checksum: len u32 (len + type + body) | type u8 | body | checksum.
fn record(salt: u64, kind: u8, body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(4 + 1 + body.len()).unwrap();
    let mut r = len.to_le_bytes().to_vec();
    r.push(kind);
    r.extend_from_slice(body);
    let sum = xxh3_64_with_seed(&r, salt);
    r.extend(sum.to_le_bytes());
    r
}

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

#[test]
fn the_history_survives_a_restart_and_a_recopy_after_it_is_a_duplicate() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    // Two types in one entry: inline text and an image in a payload file.
    daemon.copy_new(&[(UTF8, b"hello"), (PNG, &big(0))]);
    daemon.copy_new(&[(UTF8, b"world")]);
    let files = state.payload_files();
    assert_eq!(files.len(), 1, "{files:?}");
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 2 entries.");
    assert_eq!(
        state.payload_files(),
        files,
        "a restored entry keeps its file"
    );
    // The same copy again matches the restored entry: its types, sizes and hashes came back.
    daemon.copy(&[(UTF8, b"hello"), (PNG, &big(0))]);
    daemon.line_with(DUPLICATE);
    // A new copy with the same layout gets an id after every old one: its file can't collide.
    daemon.copy_new(&[(UTF8, b"hello"), (PNG, &big(1))]);
    assert_eq!(
        state.payload_files().len(),
        2,
        "{:?}",
        state.payload_files()
    );
    daemon.stop();

    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 3 entries.");
    assert_eq!(state.payload_files().len(), 2);
}

#[test]
fn a_recopied_entry_stays_newer_than_the_ones_before_it_after_a_restart() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &big(1))]); // A
    daemon.copy_new(&[(UTF8, &big(2))]); // B
    let files = state.payload_files();
    let (a, b) = (files[0].clone(), files[1].clone());
    daemon.copy(&[(UTF8, &big(1))]); // A again: now newer than B
    daemon.lines_with(&[DUPLICATE, TOUCHED]);
    daemon.stop();

    // The order isn't printed, so eviction shows it: 99 more copies push out the oldest.
    let mut daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 2 entries.");
    for i in 0..99 {
        daemon.copy_new(&[(UTF8, format!("entry {i}").as_bytes())]);
    }
    daemon.line_with(DELETED);
    assert_eq!(state.payload_files(), [a], "B ({b:?}) is the oldest");
}

#[test]
fn a_touch_of_an_unknown_entry_is_ignored_but_its_seq_is_never_reused() {
    // A Touch can outlive its entry only in a hand-made log, but its seq was handed out once.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, b"first")]);
    daemon.stop();
    let body = [999u64, 1000, 0].map(u64::to_le_bytes).concat();
    append(&state, &record(salt_of(&state.wal_bytes()), TOUCH, &body));
    let with_touch = state.wal_len();

    let mut daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert_eq!(state.wal_len(), with_touch, "a valid record is not cut");
    daemon.copy_new(&[(UTF8, &big(0))]);
    let files = state.payload_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(id_and_type(&files[0].0).0 > 1000, "{files:?}");
}

#[test]
fn an_evicted_entry_stays_gone_after_a_restart() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &big(0))]); // the oldest, with a file
    assert_eq!(state.payload_files().len(), 1);
    for i in 0..100 {
        daemon.copy_new(&[(UTF8, format!("entry {i}").as_bytes())]);
    }
    daemon.line_with(DELETED); // the 101st entry pushed the oldest out of the 100 kept
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    daemon.stop();

    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 100 entries.");
    assert!(state.payload_files().is_empty());
}

#[test]
fn a_torn_or_zero_filled_tail_is_cut_and_the_history_before_it_kept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, b"first")]);
    let after_first = state.wal_len();
    daemon.copy_new(&[(UTF8, &big(0))]);
    daemon.stop();

    // A crash in the middle of writing the last record: its file goes with it.
    set_wal_len(&state, state.wal_len() - 3);
    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert_eq!(state.wal_len(), after_first);
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
    daemon.stop();

    // A power cut can leave the file longer than what was written, the rest zeros.
    append(&state, &[0; 4096]);
    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert_eq!(state.wal_len(), after_first);
}

#[test]
fn a_damaged_record_is_dropped_with_everything_after_it() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, b"first")]);
    let after_first = state.wal_len();
    daemon.copy_new(&[(UTF8, b"second")]);
    daemon.copy_new(&[(UTF8, b"third")]);
    daemon.stop();

    let mut wal = state.wal_bytes();
    wal[after_first as usize + 6] ^= 0x40; // inside the second record's body
    fs::write(state.wal(), &wal).unwrap();
    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert_eq!(state.wal_len(), after_first);
}

#[test]
fn a_record_of_an_unknown_type_is_skipped_and_the_records_after_it_kept() {
    // What a newer build may write (2d adds Touch): an older build must not cut the log there.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, b"first")]);
    daemon.stop();
    append(
        &state,
        &record(salt_of(&state.wal_bytes()), 99, b"from the future"),
    );
    let with_unknown = state.wal_len();

    let mut daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert_eq!(state.wal_len(), with_unknown, "the unknown record stays");
    daemon.copy_new(&[(UTF8, b"second")]);
    daemon.stop();

    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 2 entries.");
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
        daemon.copy_new(&[(UTF8, &big(0))]);
        daemon.stop();
        let mut wal = state.wal_bytes();
        let old_salt = salt_of(&wal);
        damage(&mut wal);
        fs::write(state.wal(), &wal).unwrap();

        let mut daemon = Daemon::start_in(&state);
        daemon.line_with("Restored 0 entries.");
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
        daemon.copy_new(&[(UTF8, b"after")]);
        daemon.stop();
        let daemon = Daemon::start_in(&state);
        daemon.line_with("Restored 1 entries.");
    }
}

#[test]
fn an_impossible_sequence_number_starts_an_empty_history() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &big(0))]);
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
    daemon.line_with("impossible seq");
    daemon.line_with("Restored 0 entries.");
    assert!(state.payload_files().is_empty());
    assert_eq!(state.wal_len(), HEADER_LEN as u64);
    // Records written after the reset use its new salt: they survive the next restart.
    daemon.copy_new(&[(UTF8, b"after")]);
    daemon.stop();
    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
}

#[test]
fn an_entry_with_a_missing_or_short_payload_is_dropped_whole_and_strays_are_swept() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    daemon.copy_new(&[(UTF8, &big(1))]); // A: one file
    daemon.copy_new(&[(UTF8, &big(2)), (PNG, &big(3))]); // B: two files
    daemon.copy_new(&[(UTF8, b"inline")]); // C: none
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

    let daemon = Daemon::start_in(&state);
    daemon.line_with("Restored 1 entries.");
    assert!(
        state.payload_files().is_empty(),
        "{:?}",
        state.payload_files()
    );
}
