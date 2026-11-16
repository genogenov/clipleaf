//! `List` through the real `clipleafd` binary: the history newest first and paged, one summary
//! per stored content type in the daemon's order (file lists, the image, the text), with what a
//! client needs to show each one: its stored bytes as the preview, the file count, the
//! thumbnail (its JPEG bytes only when asked for), and whether a file copy is a cut. The KDE cut
//! marker is stored but not listed.

mod common;

use std::{
    io::Cursor,
    thread,
    time::{Duration, Instant},
};

use clipleaf_ipc::{
    client::Client,
    protocol::v1::models::{EntrySummary, FileOp, Kind, List, ListReply, ThumbState, TypeSummary},
};
use common::{ipc::*, *};
use image::{ImageFormat, Rgb, RgbImage};

const HTML: &str = "text/html";
const GNOME: &str = "x-special/gnome-copied-files";
const URI_LIST: &str = "text/uri-list";
const KDE_URI_LIST: &str = "application/x-kde4-urilist";
const KDE_CUT: &str = "application/x-kde-cutselection";

fn client(daemon: &mut Daemon) -> Client {
    drop(connect(daemon)); // waits until the daemon listens
    Client::connect_handshake(daemon.socket(), 4096, 64 * 1024, Some(GUARD)).expect("handshake")
}

fn list(client: &mut Client, offset: u64, limit: Option<u32>, thumbs: bool) -> ListReply {
    client
        .call(&List {
            query: None,
            offset,
            limit,
            thumbs,
        })
        .expect("List")
}

/// Lists everything until `done`: an entry reaches `List` a moment after its WAL record.
fn list_until(client: &mut Client, thumbs: bool, done: impl Fn(&ListReply) -> bool) -> ListReply {
    let start = Instant::now();
    loop {
        let reply = list(client, 0, None, thumbs);
        if done(&reply) {
            return reply;
        }
        assert!(start.elapsed() < WAIT, "List never got there: {reply:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// The listed entry `id`, once it is listed.
fn entry(client: &mut Client, id: u64) -> EntrySummary {
    let reply = list_until(client, false, |r| r.entries.iter().any(|e| e.id == id));
    reply.entries.into_iter().find(|e| e.id == id).unwrap()
}

fn mimes(entry: &EntrySummary) -> Vec<&str> {
    entry.types.iter().map(|t| t.mime.as_str()).collect()
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(width, height, |x, _| if x < width / 2 { Rgb([220, 30, 30]) } else { Rgb([30, 30, 220]) });
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

fn ids(reply: &ListReply) -> Vec<u64> {
    reply.entries.iter().map(|e| e.id).collect()
}

#[test]
fn the_history_is_listed_newest_first_with_its_total_and_pages_by_offset_and_limit() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    assert_eq!(list(&mut client, 0, None, false), ListReply { entries: vec![], total: 0 });

    let a = daemon.copy_new(&[(UTF8, b"first")]).id;
    let b = daemon.copy_new(&[(UTF8, b"second")]).id;
    let c = daemon.copy_new(&[(UTF8, b"third")]).id;
    let all = list_until(&mut client, false, |r| r.total == 3);
    assert_eq!(ids(&all), [c, b, a]);

    let page = list(&mut client, 1, Some(1), false);
    assert_eq!((ids(&page), page.total), (vec![b], 3));
    let tail = list(&mut client, 2, Some(10), false);
    assert_eq!((ids(&tail), tail.total), (vec![a], 3));
    // Past the end: an empty page, still with the total.
    let past = list(&mut client, 3, None, false);
    assert_eq!((ids(&past), past.total), (vec![], 3));
}

#[test]
fn a_text_copy_is_one_text_type_with_all_its_bytes_as_the_preview() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    let text = "line one\r\nline two ✓\n".as_bytes();
    let id = daemon.copy_new(&[(UTF8, text)]).id;
    let e = entry(&mut client, id);
    assert_eq!(
        e.types,
        [TypeSummary {
            mime: UTF8.into(),
            size: text.len() as u64,
            kind: Kind::Text,
            thumb: None,
            file_count: None,
            preview: text.to_vec(),
        }]
    );
    assert_eq!((e.pinned, e.file_op, e.score), (false, None, None));
    assert!(e.time_ms > 1_700_000_000_000, "{}", e.time_ms);

    let html = daemon.copy_new(&[(HTML, b"<b>bold</b>")]).id;
    let e = entry(&mut client, html);
    assert_eq!((mimes(&e), e.types[0].kind), (vec![HTML], Kind::Html));
    assert_eq!(e.types[0].preview, b"<b>bold</b>");
}

#[test]
fn a_text_kept_in_a_payload_file_previews_its_first_5_kib_and_reports_its_full_size() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    let text: Vec<u8> = (0..20_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let id = daemon.copy_new(&[(UTF8, &text)]).id;
    let t = &entry(&mut client, id).types[0];
    assert_eq!(t.size, 20_000);
    assert_eq!(t.preview, text[..5 * 1024]);
}

#[test]
fn a_file_copy_lists_its_file_lists_first_with_their_counts_then_its_text() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    // What Nautilus offers for a copy of three files.
    let gnome = b"copy\nfile:///a\nfile:///b%20c\nfile:///d";
    let uris = b"file:///a\r\nfile:///b%20c\r\nfile:///d\r\n";
    let id = daemon.copy_new(&[(UTF8, b"/a\n/b c\n/d"), (GNOME, gnome), (URI_LIST, uris)]).id;
    let e = entry(&mut client, id);
    assert_eq!(mimes(&e), [GNOME, URI_LIST, UTF8]);
    let shown: Vec<_> = e.types.iter().map(|t| (t.kind, t.file_count, t.preview.as_slice())).collect();
    assert_eq!(
        shown,
        [
            (Kind::File, Some(3), &gnome[..]),
            (Kind::File, Some(3), &uris[..]),
            (Kind::Text, None, &b"/a\n/b c\n/d"[..]),
        ]
    );
    assert_eq!(e.file_op, Some(FileOp::Copy));
}

#[test]
fn a_gnome_cut_is_listed_as_a_cut() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    let id = daemon.copy_new(&[(UTF8, b"/a"), (GNOME, b"cut\nfile:///a")]).id;
    assert_eq!(entry(&mut client, id).file_op, Some(FileOp::Cut));
}

#[test]
fn the_kde_cut_marker_says_cut_or_copy_and_is_not_listed_as_a_type() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    let list = b"file:///a\r\n";
    let cut = daemon.copy_new(&[(URI_LIST, list), (KDE_URI_LIST, list), (KDE_CUT, b"1")]).id;
    let copy = daemon.copy_new(&[(URI_LIST, b"file:///b\r\n"), (KDE_CUT, b"0")]).id;

    let e = entry(&mut client, cut);
    assert_eq!((mimes(&e), e.file_op), (vec![URI_LIST, KDE_URI_LIST], Some(FileOp::Cut)));
    let e = entry(&mut client, copy);
    assert_eq!((mimes(&e), e.file_op), (vec![URI_LIST], Some(FileOp::Copy)));
}

#[test]
fn an_image_copy_lists_the_image_first_and_its_thumbnail_bytes_only_when_asked() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    // What a browser offers for "Copy image".
    let image = png(600, 300);
    let id = daemon.copy_new(&[(HTML, b"<img src=\"x.png\">"), (PNG, &image)]).id;
    let thumb = daemon.wait_thumb(id).expect("a thumbnail");

    let ready = |r: &ListReply| {
        r.entries
            .iter()
            .any(|e| e.id == id && e.types[0].thumb.as_ref().is_some_and(|t| t.state == ThumbState::Ready))
    };
    let with = list_until(&mut client, true, ready);
    let e = with.entries.iter().find(|e| e.id == id).unwrap();
    assert_eq!(mimes(e), [PNG, HTML]);
    let (img, html) = (&e.types[0], &e.types[1]);
    assert_eq!((img.kind, img.size, img.file_count), (Kind::Image, image.len() as u64, None));
    assert!(img.preview.is_empty(), "an image has no preview");
    let t = img.thumb.as_ref().unwrap();
    assert_eq!(t.format.as_deref(), Some("image/jpeg"));
    assert_eq!((t.width, t.height), (Some(256), Some(128)));
    assert_eq!((t.src_width, t.src_height), (Some(600), Some(300)));
    assert_eq!((t.orientation, t.has_alpha), (Some(1), Some(false)));
    assert_eq!(t.data.as_deref(), Some(&thumb.jpeg[..]));
    // Only image types carry a thumbnail.
    assert_eq!((html.kind, &html.thumb), (Kind::Html, &None));

    // Without `thumbs`: the same state and sizes, no bytes.
    let without = list(&mut client, 0, None, false);
    let t = without.entries.iter().find(|e| e.id == id).unwrap().types[0].thumb.clone().unwrap();
    assert_eq!((t.state, t.width, t.src_width, t.data), (ThumbState::Ready, Some(256), Some(600), None));
}

#[test]
fn lists_pipelined_past_the_reply_backpressure_bound_are_all_answered_in_order() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    // 30 entries with a 5 KiB preview each: about 155 KB per reply, so 100 replies (about
    // 15 MB) are well past the daemon's 8 MiB of unsent replies per connection.
    for i in 0..30u32 {
        let text: Vec<u8> = format!("entry {i} ").bytes().cycle().take(6 * 1024).collect();
        daemon.copy_new(&[(UTF8, &text)]);
    }
    let expected = list_until(&mut client, false, |r| r.total == 30);

    // All 100 requests in one write before reading anything: the daemon has to stop reading
    // at the bound and come back to the buffered requests once the replies drain, with no
    // new input from the client.
    let mut conn = Conn::ready(&mut daemon);
    let list = List {
        query: None,
        offset: 0,
        limit: None,
        thumbs: false,
    };
    let requests: Vec<u8> = (2..102).flat_map(|req_id| frame(req_id, &list)).collect();
    conn.send(&requests);
    for req_id in 2..102 {
        assert_eq!(conn.recv_msg::<ListReply>(req_id), expected, "reply {req_id}");
    }
}

#[test]
fn an_image_that_cannot_have_a_thumbnail_is_listed_as_unavailable() {
    let mut daemon = Daemon::start();
    let mut client = client(&mut daemon);
    let id = daemon.copy_new(&[(PNG, b"not an image at all")]).id;
    assert_eq!(daemon.wait_thumb(id), None);
    let reply = list_until(&mut client, true, |r| {
        r.entries
            .iter()
            .any(|e| e.id == id && e.types[0].thumb.as_ref().is_some_and(|t| t.state != ThumbState::Pending))
    });
    let t = reply.entries[0].types[0].thumb.clone().unwrap();
    assert_eq!((t.state, t.format, t.width, t.data), (ThumbState::Unavailable, None, None, None));
}
