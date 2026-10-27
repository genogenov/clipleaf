//! File copies: every file-list type of an entry stores its item count, counted over all its
//! bytes as they are fetched (the head may hold only the first names), and the count survives a
//! restart and a compaction. The KDE cut marker is kept with the lists, after them.

mod common;

use common::{wal::*, *};

const GNOME: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";
const KDE_URI_LIST: &str = "application/x-kde4-urilist";
const KDE_CUT: &str = "application/x-kde-cutselection";

/// `(mime, items)` of each stored type, in the order they are stored.
fn counts(insert: &Insert) -> Vec<(&str, Option<u32>)> {
    insert.types.iter().map(|t| (t.mime.as_str(), t.items)).collect()
}

/// A uri-list of `n` files (CRLF, as RFC 2483 writes it), well over the 5 KiB inline limit
/// for n = 1000.
fn uri_list(n: u32) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("file:///home/user/Pictures/IMG_{i:05}.jpg\r\n").into_bytes())
        .collect()
}

#[test]
fn a_gnome_cut_stores_how_many_files_it_holds() {
    let mut daemon = Daemon::start();
    let list = b"cut\nfile:///a\nfile:///b%20c\nfile:///d";
    let entry = daemon.copy_new(&[(UTF8, b"/a\n/b c\n/d"), (GNOME, list)]);
    assert_eq!(counts(&entry), [(UTF8, None), (GNOME, Some(3))]);
    // Small enough to be kept whole: the head is the list as copied, nothing trimmed.
    assert_eq!(entry.types[1].head, list);
}

#[test]
fn a_kde_cut_keeps_its_marker_after_its_lists() {
    let mut daemon = Daemon::start();
    let list = b"file:///a\r\nfile:///b\r\n";
    let entry = daemon.copy_new(&[(UTF8, b"/a\n/b"), (KDE_CUT, b"1"), (URI_LIST, list), (KDE_URI_LIST, list)]);
    assert_eq!(
        counts(&entry),
        [(UTF8, None), (URI_LIST, Some(2)), (KDE_URI_LIST, Some(2)), (KDE_CUT, None),]
    );
    assert_eq!(entry.types[3].head, b"1");
}

#[test]
fn a_list_too_big_to_keep_inline_is_counted_over_all_its_bytes() {
    let mut daemon = Daemon::start();
    let list = uri_list(1000);
    let entry = daemon.copy_new(&[(URI_LIST, &list)]);
    let stored = &entry.types[0];
    assert!(stored.in_file);
    assert_eq!(stored.size, list.len() as u64);
    assert!(stored.head.len() < list.len(), "the head is only the start");
    assert!(list.starts_with(&stored.head));
    assert_eq!(stored.items, Some(1000));
}

#[test]
fn file_counts_survive_a_restart_and_a_compaction() {
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    // A type with a count between two others, and one in a payload file: each type is decoded
    // where the one before it ends.
    let gnome = b"copy\nfile:///a\nfile:///b\nfile:///c";
    let uris = b"file:///a\r\nfile:///b\r\nfile:///c\r\n";
    let nautilus = [(UTF8, &b"/a\n/b\n/c"[..]), (GNOME, gnome), (URI_LIST, uris)];
    let big = uri_list(1000);
    let a = daemon.copy_new(&nautilus);
    let b = daemon.copy_new(&[(URI_LIST, &big), (KDE_CUT, b"0")]);
    let c = daemon.copy_new(&[(UTF8, b"after the lists")]);
    daemon.stop();
    state.append_dead(1024 * 1024 + 4096);
    let old_salt = salt_of(&state.wal_bytes());

    // The compacted log is written from the history the replay rebuilt.
    let mut daemon = Daemon::start_in(&state);
    let records = daemon.wait_compacted(old_salt);
    let insert = |id: u64| {
        records
            .iter()
            .find_map(|r| match r {
                Record::Insert(i) if i.id == id => Some(i.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no Insert for {id} in {records:?}"))
    };
    assert_eq!(counts(&insert(a.id)), [(UTF8, None), (GNOME, Some(3)), (URI_LIST, Some(3))]);
    assert_eq!(counts(&insert(b.id)), [(URI_LIST, Some(1000)), (KDE_CUT, None)]);
    assert_eq!(counts(&insert(c.id)), [(UTF8, None)]);
    assert_eq!(insert(a.id).sizes(), a.sizes());
    assert_eq!(insert(b.id).sizes(), b.sizes());
    daemon.stop();

    // And the compacted log replays: each entry is still a duplicate of itself.
    let mut daemon = Daemon::start_in(&state);
    assert_eq!(daemon.copy_dup(&[(UTF8, b"after the lists")]), c.id);
    assert_eq!(daemon.copy_dup(&[(URI_LIST, &big), (KDE_CUT, b"0")]), b.id);
    assert_eq!(daemon.copy_dup(&nautilus), a.id);
}

#[test]
fn a_record_with_an_unknown_meta_tag_is_dropped_with_everything_after_it() {
    // Its checksum is valid, but the bytes after the tag can't be read without knowing it.
    let state = StateDir::new();
    let mut daemon = Daemon::start_in(&state);
    let first = daemon.copy_new(&[(UTF8, b"first")]);
    let after_first = state.wal_len() as usize;
    daemon.copy_new(&[(UTF8, b"second")]);
    daemon.stop();

    let wal = state.wal_bytes();
    let second = &wal[after_first..];
    let len = u32::from_le_bytes(second[..4].try_into().unwrap()) as usize;
    let mut body = second[5..len].to_vec();
    *body.last_mut().unwrap() = 9; // the only type's meta tag (0–3 are known)
    let mut damaged = wal[..after_first].to_vec();
    damaged.extend(record(salt_of(&wal), INSERT, &body));
    std::fs::write(state.wal(), damaged).unwrap();

    let mut daemon = Daemon::start_in(&state);
    assert_eq!(state.wal_len() as usize, after_first);
    assert_eq!(daemon.copy_dup(&[(UTF8, b"first")]), first.id);
    assert!(!daemon.has(&[(UTF8, b"second")]));
}
