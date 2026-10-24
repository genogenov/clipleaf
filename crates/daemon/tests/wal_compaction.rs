//! The log stops growing: once it holds at least 1 MiB of dead bytes (records of entries that
//! are gone, overwritten or unknown) and no fewer dead than live bytes, `clipleafd` rewrites it
//! as one Insert per live entry under a new salt and renames it over `wal.log`
//! (current-step.md 2d item 6). Dead bytes are made here as records of an unknown type, which
//! the replay skips and the compaction drops like any other dead record.

mod common;

use std::{collections::HashMap, fs};

use common::{wal::*, *};

const MIB: u64 = 1024 * 1024;
/// len + type + id + seq + time_ms + checksum.
const TOUCH_LEN: u64 = 4 + 1 + 3 * 8 + 8;
/// More than the 5 KiB inline limit, so the type is kept in a payload file.
fn big(seed: u8) -> Vec<u8> {
    (0..6000u32).map(|i| (i % 251) as u8 ^ seed).collect()
}

fn text(t: &[u8]) -> [(&str, &[u8]); 1] {
    [(UTF8, t)]
}

/// The Inserts of a compacted log by id; anything else in it fails the test.
fn inserts_by_id(records: Vec<Record>) -> HashMap<u64, Insert> {
    records
        .into_iter()
        .map(|r| match r {
            Record::Insert(insert) => (insert.id, insert),
            other => panic!("a compacted log holds only Inserts, found {other:?}"),
        })
        .collect()
}

#[test]
fn a_log_that_is_mostly_dead_is_rewritten_at_startup_as_its_live_entries() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let a = daemon.copy_new(&text(b"first"));
    let b = daemon.copy_new(&text(&big(0))); // with a payload file
    daemon.copy(&text(b"first")); // A again: newer than B
    let Record::Touch { seq: touched, .. } = daemon.next_record() else {
        panic!("expected a Touch");
    };
    daemon.stop();
    state.append_dead(MIB + 4096);
    let old_salt = salt_of(&state.wal_bytes());

    let mut daemon = Daemon::start_in(&state);
    let inserts = inserts_by_id(daemon.wait_compacted(old_salt));
    assert_eq!(inserts.len(), 2);
    assert_eq!(
        inserts[&a.id].seq, touched,
        "the Touch is folded into A's Insert"
    );
    assert_eq!(inserts[&b.id].sizes(), b.sizes());
    assert!(inserts[&b.id].types[0].in_file);
    let max_seq = inserts.values().map(|i| i.id.max(i.seq)).max().unwrap();
    let floor = seq_snapshot_of(&state.wal_bytes());
    assert!(
        floor > max_seq,
        "the new header keeps the seq floor: {floor} <= {max_seq}"
    );
    assert!(state.wal_len() < 64 * 1024, "{} bytes", state.wal_len());
    assert_eq!(state.payload_files(), [(format!("{}.0", b.id), 6000)]);
    // Records after the compaction are checksummed with the new salt: the next one is read.
    let c = daemon.copy_new(&text(b"after"));
    assert!(c.id > max_seq, "{} after {max_seq}", c.id);
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&text(b"after")), c.id);
    assert_eq!(daemon.copy_dup(&text(&big(0))), b.id);
    assert_eq!(daemon.copy_dup(&text(b"first")), a.id);
}

#[test]
fn a_running_daemon_compacts_once_the_dead_bytes_reach_1_mib() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let a = daemon.copy_new(&text(b"first"));
    daemon.stop();
    // One Touch short of 1 MiB dead: the Touch of a duplicate below makes it exactly 1 MiB.
    state.append_dead(MIB - TOUCH_LEN);
    let old_salt = salt_of(&state.wal_bytes());

    let mut daemon = Daemon::start_in(&state);
    // A new entry adds as many live bytes as log bytes: still below the limit.
    let b = daemon.copy_new(&text(b"second"));
    assert_eq!(salt_of(&state.wal_bytes()), old_salt, "compacted too early");
    daemon.copy(&text(b"first"));
    let inserts = inserts_by_id(daemon.wait_compacted(old_salt));
    assert_eq!(inserts.len(), 2);
    assert!(
        inserts[&a.id].seq > inserts[&b.id].seq,
        "the Touch that tipped it over is in the compacted log"
    );
    daemon.stop();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&text(b"second")), b.id);
    assert_eq!(daemon.copy_dup(&text(b"first")), a.id);
}

#[test]
fn a_temporary_log_left_by_a_crash_mid_compaction_is_removed_at_startup() {
    // It holds a copy of the history, inline text included.
    let state = StateDir::new();
    let daemon = Daemon::start_in(&state);
    daemon.stop();
    let tmp = state.root().join("clipleaf/wal.log.tmp");
    fs::write(&tmp, b"half a compaction").unwrap();

    let _daemon = Daemon::start_in(&state);
    assert!(!tmp.exists());
}
