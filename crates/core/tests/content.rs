//! What a stored entry is (`ContentKinds`: one kind per stored type, no order, no UI policy) and
//! how many items a file list holds (`FileListScan`: fed in pieces of any size, as a fetch reads
//! them).

use clipleaf_core::{ContentKind, ContentKinds, FileListFormat, FileListScan, classify_mime};

fn kinds(mimes: &[&str]) -> Vec<ContentKind> {
    let mimes: Vec<_> = mimes.iter().map(|m| classify_mime(*m)).collect();
    ContentKinds::of(mimes.iter()).iter().collect()
}

/// Same kinds, in any order, each once.
fn assert_kinds(mimes: &[&str], expected: &[ContentKind]) {
    let got = kinds(mimes);
    let mut sorted_got: Vec<String> = got.iter().map(|k| format!("{k:?}")).collect();
    let mut sorted_expected: Vec<String> = expected.iter().map(|k| format!("{k:?}")).collect();
    sorted_got.sort();
    sorted_expected.sort();
    assert_eq!(sorted_got, sorted_expected, "kinds of {mimes:?}");
    let set = {
        let m: Vec<_> = mimes.iter().map(|m| classify_mime(*m)).collect();
        ContentKinds::of(m.iter())
    };
    for kind in expected {
        assert!(set.has(*kind), "{mimes:?} has {kind:?}");
    }
}

#[test]
fn each_stored_type_adds_its_own_kind_and_the_ui_decides_what_they_mean() {
    use ContentKind::*;
    assert_kinds(&["text/plain;charset=utf-8"], &[Text]);
    // Plain characters whatever the encoding or text/* subtype.
    assert_kinds(&["UTF8_STRING", "text/plain;charset=utf-16", "text/csv"], &[Text]);
    // Formats with markup of their own are their own kinds, not Text.
    assert_kinds(&["text/html"], &[Html]);
    assert_kinds(&["text/markdown"], &[Markdown]);
    // A browser image copy, a file manager copy.
    assert_kinds(&["text/html", "image/png"], &[Html, Image]);
    assert_kinds(
        &["text/plain;charset=utf-8", "x-special/gnome-copied-files", "text/uri-list"],
        &[Text, File],
    );
    assert_kinds(&["image/webp"], &[Image]);
    assert_kinds(&["application/x-foo"], &[Other]);
    assert_kinds(&["x-kde-passwordManagerHint"], &[Password]);
}

#[test]
fn the_kde_cut_marker_alone_is_no_kind() {
    // It is a flag on a file list, not content.
    assert_kinds(&["application/x-kde-cutselection"], &[]);
    assert_kinds(&[], &[]);
    assert_kinds(&["text/uri-list", "application/x-kde-cutselection"], &[ContentKind::File]);
}

#[test]
fn every_kind_is_yielded_once_including_the_highest() {
    use ContentKind::*;
    let all = [
        "text/plain",
        "image/png",
        "text/uri-list",
        "x-kde-passwordManagerHint",
        "text/markdown",
        "text/html",
        "application/x-foo",
    ];
    assert_eq!(kinds(&all).len(), 7);
    assert_kinds(&all, &[Text, Image, File, Password, Markdown, Html, Other]);
    // A set of one: its only kind is also its highest.
    assert_eq!(kinds(&["application/x-foo"]).len(), 1);
    assert_eq!(kinds(&["text/plain"]).len(), 1);
}

#[test]
fn only_file_list_types_are_scanned() {
    use FileListFormat::*;
    assert_eq!(FileListFormat::from_mime("text/uri-list"), Some(UriList));
    assert_eq!(FileListFormat::from_mime("application/x-kde4-urilist"), Some(UriList));
    assert_eq!(FileListFormat::from_mime("x-special/gnome-copied-files"), Some(GnomeCopiedFiles));
    assert_eq!(FileListFormat::from_mime("application/x-kde-cutselection"), None);
    assert_eq!(FileListFormat::from_mime("text/plain"), None);
}

fn count(format: FileListFormat, pieces: &[&[u8]]) -> u32 {
    let mut scan = FileListScan::new(format);
    for piece in pieces {
        scan.feed(piece);
    }
    scan.finish()
}

/// The count of `list` fed whole, and the same count for every way to cut it in two and in
/// three pieces (a fetch reads whatever the pipe holds).
fn count_any_split(format: FileListFormat, list: &[u8]) -> u32 {
    let whole = count(format, &[list]);
    for i in 0..=list.len() {
        let (a, b) = list.split_at(i);
        assert_eq!(count(format, &[a, b]), whole, "split at {i} of {list:?}");
        for j in i..=list.len() {
            let (b, c) = list[i..].split_at(j - i);
            assert_eq!(count(format, &[a, b, c]), whole, "split at {i}, {j} of {list:?}");
        }
    }
    let bytes: Vec<&[u8]> = list.chunks(1).collect();
    assert_eq!(count(format, &bytes), whole, "byte by byte: {list:?}");
    whole
}

#[test]
fn a_gnome_list_counts_its_files_but_not_its_copy_or_cut_line() {
    use FileListFormat::GnomeCopiedFiles as G;
    // Nautilus and Thunar write no newline after the last file.
    assert_eq!(count_any_split(G, b"cut\nfile:///a\nfile:///b%20c\nfile:///d"), 3);
    assert_eq!(count_any_split(G, b"copy\nfile:///a\nfile:///b\n"), 2);
    assert_eq!(count_any_split(G, b"copy\nfile:///a"), 1);
    assert_eq!(count_any_split(G, b"copy"), 0);
    assert_eq!(count_any_split(G, b"copy\n"), 0);
    assert_eq!(count(G, &[]), 0);
    // `#` is no comment here: it is a file name like any other.
    assert_eq!(count_any_split(G, b"copy\n#file\n"), 1);
}

#[test]
fn a_uri_list_counts_its_lines_but_not_comments_or_empty_lines() {
    use FileListFormat::UriList as U;
    // RFC 2483: CRLF line ends, `#` comments.
    assert_eq!(count_any_split(U, b"# from dolphin\r\nfile:///a\r\nfile:///b\r\n"), 2);
    // LF only, no newline after the last one, empty and CR-only lines.
    assert_eq!(count_any_split(U, b"\nfile:///a\n\n\r\n\rfile:///b"), 2);
    // Any URI is an item; nothing is checked or decoded.
    assert_eq!(count_any_split(U, b"trash:///x\nsmb://host/share\n/not-a-uri\n"), 3);
    assert_eq!(count_any_split(U, b"\r\n\r\n"), 0);
    assert_eq!(count_any_split(U, b"# only a comment"), 0);
    // The first line is an item here (only GNOME's first line is the copy/cut line).
    assert_eq!(count_any_split(U, b"copy\nfile:///a"), 2);
}

#[test]
fn a_long_list_is_counted_over_all_its_bytes() {
    let mut list = Vec::new();
    for i in 0..10_000 {
        list.extend_from_slice(format!("file:///home/user/{i:05}.jpg\r\n").as_bytes());
    }
    let pieces: Vec<&[u8]> = list.chunks(4096).collect();
    assert_eq!(count(FileListFormat::UriList, &pieces), 10_000);
    let odd: Vec<&[u8]> = list.chunks(7).collect();
    assert_eq!(count(FileListFormat::UriList, &odd), 10_000);
}

#[test]
fn random_bytes_in_random_pieces_never_panic() {
    // xorshift64*: deterministic, so a failure reproduces.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    for _ in 0..2000 {
        let len = (next() % 300) as usize;
        // Mostly the bytes that matter, so every state is reached.
        let bytes: Vec<u8> = (0..len)
            .map(|_| match next() % 6 {
                0 => b'\n',
                1 => b'\r',
                2 => b'#',
                _ => next() as u8,
            })
            .collect();
        for format in [FileListFormat::UriList, FileListFormat::GnomeCopiedFiles] {
            let mut scan = FileListScan::new(format);
            let mut rest = &bytes[..];
            while !rest.is_empty() {
                let n = 1 + (next() as usize % rest.len());
                scan.feed(&rest[..n]);
                rest = &rest[n..];
            }
            let pieces = scan.finish();
            assert_eq!(pieces, count(format, &[&bytes]), "{bytes:?}");
            assert!(pieces as usize <= bytes.len());
        }
    }
}
