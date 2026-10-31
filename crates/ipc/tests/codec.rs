//! encode_frame / decode_body: round trips for every message, wire format, forward
//! compatibility, enum fallbacks and hostile bodies.

mod common;

use std::{collections::BTreeMap, fmt::Debug};

use clipleaf_ipc::{
    frame::frame_reader::FrameReader,
    protocol::{
        Message,
        codec::{decode_body, encode_frame},
        error::ProtocolError,
        handshake::{ErrorCode, ErrorReply, Hello, HelloReply},
        header::{EventType, HEADER_LEN, MAX_FRAME_LEN, MessageType, RequestType},
        v1::models::*,
    },
};
use common::{ChunkReader, XorShift, body_of, read_all_frames, sample_entry};
use serde::{Deserialize, Serialize};

// ---- Round trips ----

fn round_trip<T: Message + PartialEq + Debug>(msg: T, wire_type: [u8; 2]) {
    assert_eq!(T::TYPE.to_wire(), wire_type, "{msg:?}");
    for req_id in [0u32, 1, 0xDEAD_BEEF, u32::MAX] {
        let mut buf = Vec::new();
        encode_frame(req_id, &msg, &mut buf, MAX_FRAME_LEN).unwrap();
        assert_eq!(&buf[4..6], &wire_type);
        let mut r = FrameReader::new(0, MAX_FRAME_LEN as usize);
        let frames = read_all_frames(&mut r, &mut ChunkReader::new(buf.clone(), vec![]));
        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert_eq!(f.msg_type, T::TYPE);
        assert_eq!(f.req_id, req_id);
        assert_eq!((f.nfds, f.flags), (0, 0));
        assert_eq!(f.len as usize, buf.len() - HEADER_LEN);
        assert_eq!(decode_body::<T>(&f.body).unwrap(), msg);
    }
}

#[test]
fn every_message_round_trips() {
    use RequestType as R;
    let req = |r: R| MessageType::Request(r).to_wire();
    let rep = |r: R| MessageType::Reply(r).to_wire();
    assert_eq!(req(R::Hello), [1, 1]);

    round_trip(
        Hello {
            versions: vec![1, 2, u32::MAX],
            features: vec!["fds".into(), "".into()],
        },
        [1, 1],
    );
    round_trip(
        HelloReply {
            version: 1,
            features: vec!["x".into()],
            daemon: Some("clipleafd 0.1.0".into()),
        },
        [2, 1],
    );
    round_trip(
        HelloReply {
            version: 7,
            features: vec![],
            daemon: None,
        },
        [2, 1],
    );
    for code in [
        ErrorCode::UnknownRequest,
        ErrorCode::BadRequest,
        ErrorCode::NotFound,
        ErrorCode::UnsupportedVersion,
        ErrorCode::Internal,
        ErrorCode::Cancelled,
        ErrorCode::Generic,
    ] {
        round_trip(
            ErrorReply {
                code,
                message: "msg ✓".into(),
            },
            [3, 0],
        );
    }
    round_trip(Status {}, req(R::Status));
    round_trip(
        StatusReply {
            entries: u64::MAX,
            pinned: 3,
            history_limit: 1000,
            clipboard_id: Some(42),
            primary_id: None,
        },
        rep(R::Status),
    );
    round_trip(
        List {
            query: Some("needle".into()),
            offset: 100,
            limit: Some(50),
            thumbs: true,
        },
        req(R::List),
    );
    round_trip(
        List {
            query: None,
            offset: 0,
            limit: None,
            thumbs: false,
        },
        req(R::List),
    );
    let mut bare = sample_entry(2);
    bare.kind = Kind::Text;
    bare.thumb = None;
    bare.file_count = None;
    bare.file_op = None;
    bare.score = None;
    bare.types.clear();
    bare.preview.clear();
    round_trip(
        ListReply {
            entries: vec![sample_entry(1), bare],
            total: 2,
        },
        rep(R::List),
    );
    round_trip(ListReply { entries: vec![], total: 0 }, rep(R::List));
    for kind in [Kind::Text, Kind::Html, Kind::Markdown, Kind::Image, Kind::File, Kind::Other] {
        for state in [ThumbState::Ready, ThumbState::Pending, ThumbState::Unavailable] {
            for op in [FileOp::Cut, FileOp::Copy] {
                let mut e = sample_entry(9);
                e.kind = kind;
                e.thumb.as_mut().unwrap().state = state;
                e.thumb.as_mut().unwrap().data = None;
                e.file_op = Some(op);
                round_trip(Added { entry: e.clone() }, [4, 1]);
            }
        }
    }
    for selection in [Selection::Clipboard, Selection::Primary, Selection::Both] {
        for formats in [Formats::All, Formats::Text, Formats::Mime] {
            round_trip(
                Restore {
                    id: 1,
                    selection,
                    formats,
                    mime: Some("text/html".into()),
                },
                req(R::Restore),
            );
        }
    }
    round_trip(RestoreReply {}, rep(R::Restore));
    round_trip(Pin { id: 3, pinned: true }, req(R::Pin));
    round_trip(PinReply {}, rep(R::Pin));
    round_trip(Delete { id: u64::MAX }, req(R::Delete));
    round_trip(DeleteReply {}, rep(R::Delete));
    round_trip(Clear { include_pinned: true }, req(R::Clear));
    round_trip(ClearReply { removed: 17 }, rep(R::Clear));
    round_trip(Subscribe { thumbs: true }, req(R::Subscribe));
    round_trip(SubscribeReply {}, rep(R::Subscribe));
    round_trip(Added { entry: sample_entry(1) }, [4, 1]);
    round_trip(Updated { entry: sample_entry(2) }, [4, 2]);
    round_trip(Removed { id: 3 }, [4, 3]);
    round_trip(Cleared {}, [4, 4]);
    for selection in [EventSelection::Clipboard, EventSelection::Primary] {
        round_trip(SelectionChanged { selection, id: Some(4) }, [4, 5]);
        round_trip(SelectionChanged { selection, id: None }, [4, 5]);
    }
    assert_eq!(MessageType::Event(EventType::SelectionChanged).to_wire(), [4, 5]);
}

#[test]
fn many_messages_in_one_buffer_parse_in_order() {
    let mut buf = Vec::new();
    encode_frame(1, &Status {}, &mut buf, MAX_FRAME_LEN).unwrap();
    encode_frame(0, &Added { entry: sample_entry(7) }, &mut buf, MAX_FRAME_LEN).unwrap();
    encode_frame(2, &Delete { id: 9 }, &mut buf, MAX_FRAME_LEN).unwrap();
    let mut r = FrameReader::new(0, MAX_FRAME_LEN as usize);
    let f = read_all_frames(&mut r, &mut ChunkReader::new(buf, vec![]));
    assert_eq!(f.len(), 3);
    assert_eq!((f[0].msg_type, f[0].req_id), (Status::TYPE, 1));
    assert_eq!(decode_body::<Status>(&f[0].body).unwrap(), Status {});
    assert_eq!((f[1].msg_type, f[1].req_id), (Added::TYPE, 0));
    assert_eq!(decode_body::<Added>(&f[1].body).unwrap().entry, sample_entry(7));
    assert_eq!((f[2].msg_type, f[2].req_id), (Delete::TYPE, 2));
    assert_eq!(decode_body::<Delete>(&f[2].body).unwrap(), Delete { id: 9 });
}

// ---- encode_frame buffer handling ----

#[test]
fn encode_frame_appends_after_existing_content() {
    let mut buf = b"prefix".to_vec();
    encode_frame(3, &Delete { id: 5 }, &mut buf, MAX_FRAME_LEN).unwrap();
    assert_eq!(&buf[..6], b"prefix");
    let frame = &buf[6..];
    assert_eq!(&frame[..HEADER_LEN], &[5, 0, 0, 0, 1, 6, 0, 0, 3, 0, 0, 0]);
    assert_eq!(&frame[HEADER_LEN..], &[0x81, 0xa2, b'i', b'd', 5]);
}

#[test]
fn encode_frame_over_cap_truncates_back_to_prior_content() {
    let msg = Delete { id: 5 }; // frame = 12 + 5 = 17 bytes
    let mut buf = b"prior".to_vec();
    encode_frame(1, &msg, &mut buf, 17).unwrap();
    assert_eq!(buf.len(), 5 + 17);

    let mut buf = b"prior".to_vec();
    let res = encode_frame(1, &msg, &mut buf, 16);
    assert!(matches!(res, Err(ProtocolError::FrameTooBig)), "{res:?}");
    assert_eq!(buf, b"prior");

    let mut buf = Vec::new();
    let mut entry = sample_entry(1);
    entry.preview = vec![0; 70_000];
    let big = Added { entry };
    assert!(matches!(encode_frame(1, &big, &mut buf, 64 * 1024), Err(ProtocolError::FrameTooBig)));
    assert!(buf.is_empty());
}

// ---- Wire format pins ----

#[test]
fn empty_structs_encode_as_empty_map() {
    assert_eq!(body_of(&Status {}), [0x80]);
    assert_eq!(body_of(&RestoreReply {}), [0x80]);
    assert_eq!(body_of(&PinReply {}), [0x80]);
    assert_eq!(body_of(&DeleteReply {}), [0x80]);
    assert_eq!(body_of(&SubscribeReply {}), [0x80]);
    assert_eq!(body_of(&Cleared {}), [0x80]);
    // and decode from an empty map
    decode_body::<Status>(&[0x80]).unwrap();
    decode_body::<Cleared>(&[0x80]).unwrap();
}

#[test]
fn bodies_are_named_maps_with_snake_case_strings_and_bin_bytes() {
    let mut expected = vec![0x82, 0xa9];
    expected.extend(b"selection");
    expected.push(0xa7);
    expected.extend(b"primary");
    expected.extend([0xa2, b'i', b'd', 0xc0]);
    assert_eq!(
        body_of(&SelectionChanged {
            selection: EventSelection::Primary,
            id: None
        }),
        expected
    );

    // Byte fields are bin (0xc4 len bytes), not arrays: the preview and the thumbnail data.
    let mut entry = sample_entry(1);
    entry.preview = vec![9, 9];
    entry.thumb.as_mut().unwrap().data = Some(vec![7, 7, 7]);
    let body = body_of(&Added { entry });
    let mut preview = vec![0xa7];
    preview.extend(b"preview");
    preview.extend([0xc4, 2, 9, 9]);
    let mut data = vec![0xa4];
    data.extend(b"data");
    data.extend([0xc4, 3, 7, 7, 7]);
    for field in [preview, data] {
        assert!(body.windows(field.len()).any(|w| w == field), "{field:x?} not in {body:x?}");
    }

    let mut expected = vec![0x82, 0xa4];
    expected.extend(b"code");
    expected.push(0xa9);
    expected.extend(b"not_found");
    expected.push(0xa7);
    expected.extend(b"message");
    expected.push(0xa0);
    assert_eq!(
        body_of(&ErrorReply {
            code: ErrorCode::NotFound,
            message: String::new()
        }),
        expected
    );
}

// ---- Forward compatibility (test-side mirror types) ----

macro_rules! test_message {
    ($($ty:ty => $t:expr;)*) => {
        $(impl Message for $ty { const TYPE: MessageType = $t; })*
    };
}

/// v1 `Delete` plus unknown fields of every shape, placed before the known one.
#[derive(Serialize, Deserialize)]
struct DeleteV2 {
    reason: String,
    nested: Vec<Vec<u32>>,
    map: BTreeMap<String, Option<bool>>,
    #[serde(with = "serde_bytes")]
    blob: Vec<u8>,
    float: f64,
    id: u64,
    tail: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RawThumb {
    glow: u32,
    state: String,
    width: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RawEntry {
    id: u64,
    kind: String,
    time_ms: u64,
    pinned: bool,
    size: u64,
    #[serde(with = "serde_bytes")]
    preview: Vec<u8>,
    types: Vec<TypeSummary>,
    thumb: Option<RawThumb>,
    file_count: Option<u32>,
    file_op: Option<String>,
    score: Option<f32>,
    new_field: BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct RawListReply {
    cursor: Option<u64>,
    entries: Vec<RawEntry>,
    total: u64,
}

#[derive(Serialize, Deserialize)]
struct RawAdded {
    entry: RawEntry,
}

#[derive(Serialize, Deserialize)]
struct RawError {
    code: String,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct RawSelectionChanged {
    selection: String,
    id: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct RawRestore {
    id: u64,
    selection: String,
    formats: String,
    mime: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct HelloV0 {
    versions: Vec<u32>,
}

#[derive(Serialize, Deserialize)]
struct HelloReplyV0 {
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct ErrorV0 {
    code: String,
}

#[derive(Serialize, Deserialize)]
struct StatusReplyPartial {
    entries: u64,
    pinned: u64,
}

#[derive(Serialize, Deserialize)]
struct RestoreNoSelection {
    id: u64,
    formats: String,
}

test_message! {
    DeleteV2 => MessageType::Request(RequestType::Delete);
    RawListReply => MessageType::Reply(RequestType::List);
    RawAdded => MessageType::Event(EventType::Added);
    RawError => MessageType::Error;
    RawSelectionChanged => MessageType::Event(EventType::SelectionChanged);
    RawRestore => MessageType::Request(RequestType::Restore);
    HelloV0 => MessageType::Request(RequestType::Hello);
    HelloReplyV0 => MessageType::Reply(RequestType::Hello);
    ErrorV0 => MessageType::Error;
    StatusReplyPartial => MessageType::Reply(RequestType::Status);
    RestoreNoSelection => MessageType::Request(RequestType::Restore);
}

fn raw_entry(kind: &str, file_op: Option<&str>, thumb_state: &str) -> RawEntry {
    RawEntry {
        id: 77,
        kind: kind.into(),
        time_ms: 5,
        pinned: true,
        size: 10,
        preview: b"hi\xff".to_vec(),
        types: vec![TypeSummary {
            mime: "text/plain".into(),
            size: 10,
        }],
        thumb: Some(RawThumb {
            glow: 9,
            state: thumb_state.into(),
            width: Some(64),
        }),
        file_count: None,
        file_op: file_op.map(Into::into),
        score: Some(1.5),
        new_field: BTreeMap::from([("a".into(), 1), ("b".into(), u64::MAX)]),
    }
}

#[test]
fn superset_struct_decodes_ignoring_unknown_fields() {
    let v2 = DeleteV2 {
        reason: "because".into(),
        nested: vec![vec![1, 2], vec![], vec![u32::MAX]],
        map: BTreeMap::from([("k".into(), None), ("j".into(), Some(true))]),
        blob: vec![0xc1; 300],
        float: 2.5,
        id: 31337,
        tail: Some("t".into()),
    };
    assert_eq!(decode_body::<Delete>(&body_of(&v2)).unwrap(), Delete { id: 31337 });
}

#[test]
fn nested_superset_with_missing_optional_fields_decodes() {
    let raw = RawListReply {
        cursor: Some(4),
        entries: vec![raw_entry("text", Some("cut"), "pending")],
        total: 1,
    };
    let got = decode_body::<ListReply>(&body_of(&raw)).unwrap();
    assert_eq!(got.total, 1);
    let e = &got.entries[0];
    assert_eq!((e.id, e.kind, e.pinned, e.size), (77, Kind::Text, true, 10));
    assert_eq!(e.preview, b"hi\xff");
    assert_eq!(e.file_op, Some(FileOp::Cut));
    assert_eq!(e.score, Some(1.5));
    let t = e.thumb.as_ref().unwrap();
    assert_eq!(t.state, ThumbState::Pending);
    assert_eq!(t.width, Some(64));
    // fields absent on the wire: Option -> None, defaulted bytes -> None
    assert_eq!(
        (t.format.clone(), t.height, t.orientation, t.has_alpha, t.data.clone()),
        (None, None, None, None, None)
    );
}

#[test]
fn missing_defaulted_and_optional_fields_decode() {
    // {} as List / Clear / Subscribe: every field defaults.
    assert_eq!(
        decode_body::<List>(&[0x80]).unwrap(),
        List {
            query: None,
            offset: 0,
            limit: None,
            thumbs: false
        }
    );
    assert_eq!(decode_body::<Clear>(&[0x80]).unwrap(), Clear { include_pinned: false });
    assert_eq!(decode_body::<Subscribe>(&[0x80]).unwrap(), Subscribe { thumbs: false });
    assert_eq!(
        decode_body::<Hello>(&body_of(&HelloV0 { versions: vec![1] })).unwrap(),
        Hello {
            versions: vec![1],
            features: vec![]
        }
    );
    assert_eq!(
        decode_body::<HelloReply>(&body_of(&HelloReplyV0 { version: 1 })).unwrap(),
        HelloReply {
            version: 1,
            features: vec![],
            daemon: None
        }
    );
    assert_eq!(
        decode_body::<ErrorReply>(&body_of(&ErrorV0 { code: "internal".into() })).unwrap(),
        ErrorReply {
            code: ErrorCode::Internal,
            message: String::new()
        }
    );
    // An optional bin field (serde_bytes) that is missing: RawThumb has no `data`.
    let old = decode_body::<Added>(&body_of(&RawAdded {
        entry: raw_entry("image", None, "ready"),
    }))
    .unwrap();
    assert_eq!(old.entry.thumb.unwrap().data, None);
    // Option fields missing
    assert_eq!(
        decode_body::<StatusReply>(&body_of(&StatusReply {
            entries: 1,
            pinned: 2,
            history_limit: 3,
            clipboard_id: None,
            primary_id: None
        }))
        .unwrap()
        .clipboard_id,
        None
    );
}

#[test]
fn missing_required_field_is_an_error() {
    let partial = body_of(&StatusReplyPartial { entries: 1, pinned: 2 });
    assert!(matches!(decode_body::<StatusReply>(&partial), Err(ProtocolError::Decode(_))));
    assert!(matches!(decode_body::<Delete>(&[0x80]), Err(ProtocolError::Decode(_))));
    assert!(matches!(decode_body::<Hello>(&[0x80]), Err(ProtocolError::Decode(_))));
    assert!(matches!(decode_body::<ErrorReply>(&[0x80]), Err(ProtocolError::Decode(_))));
    let no_sel = body_of(&RestoreNoSelection {
        id: 1,
        formats: "all".into(),
    });
    assert!(matches!(decode_body::<Restore>(&no_sel), Err(ProtocolError::Decode(_))));
}

#[test]
fn unknown_enum_values_fall_back_where_safe() {
    let added = RawAdded {
        entry: raw_entry("video", Some("move"), "exploded"),
    };
    let got = decode_body::<Added>(&body_of(&added)).unwrap().entry;
    assert_eq!(got.kind, Kind::Other);
    assert_eq!(got.file_op, Some(FileOp::Copy));
    assert_eq!(got.thumb.unwrap().state, ThumbState::Unavailable);

    // known values still map exactly
    let known = decode_body::<Added>(&body_of(&RawAdded {
        entry: raw_entry("markdown", Some("copy"), "ready"),
    }))
    .unwrap()
    .entry;
    assert_eq!(
        (known.kind, known.file_op, known.thumb.unwrap().state),
        (Kind::Markdown, Some(FileOp::Copy), ThumbState::Ready)
    );

    let err = decode_body::<ErrorReply>(&body_of(&RawError {
        code: "rate_limited".into(),
        message: "slow down".into(),
    }))
    .unwrap();
    assert_eq!(
        err,
        ErrorReply {
            code: ErrorCode::Generic,
            message: "slow down".into()
        }
    );
}

#[test]
fn unknown_selection_or_formats_is_an_error() {
    // Events fall back instead: a newer daemon's selection decodes as Other, which a client skips.
    let sel = RawSelectionChanged {
        selection: "secondary".into(),
        id: Some(1),
    };
    assert_eq!(decode_body::<SelectionChanged>(&body_of(&sel)).unwrap().selection, EventSelection::Other);
    let ok = RawSelectionChanged {
        selection: "clipboard".into(),
        id: Some(1),
    };
    assert_eq!(
        decode_body::<SelectionChanged>(&body_of(&ok)).unwrap().selection,
        EventSelection::Clipboard
    );

    let restore = |selection: &str, formats: &str| {
        decode_body::<Restore>(&body_of(&RawRestore {
            id: 1,
            selection: selection.into(),
            formats: formats.into(),
            mime: None,
        }))
    };
    assert!(restore("both", "mime").is_ok());
    assert!(matches!(restore("both", "everything"), Err(ProtocolError::Decode(_))));
    assert!(matches!(restore("secondary", "all"), Err(ProtocolError::Decode(_))));
    assert!(matches!(restore("", "all"), Err(ProtocolError::Decode(_))));
}

// ---- Hostile bodies ----

fn hostile<T: Message>(valid: &[u8], seed: u64) {
    assert!(decode_body::<T>(valid).is_ok());
    // every truncation
    for n in 0..valid.len() {
        let _ = decode_body::<T>(&valid[..n]);
    }
    let mut rng = XorShift::new(seed);
    // random byte strings
    for _ in 0..2000 {
        let len = rng.below(64);
        let _ = decode_body::<T>(&rng.bytes(len));
    }
    // random byte flips of the valid body
    for _ in 0..2000 {
        let mut b = valid.to_vec();
        for _ in 0..1 + rng.below(4) {
            let i = rng.below(b.len());
            b[i] = rng.next_u64() as u8;
        }
        let _ = decode_body::<T>(&b);
    }
}

#[test]
fn hostile_bodies_never_panic() {
    let entry = sample_entry(3);
    hostile::<Hello>(
        &body_of(&Hello {
            versions: vec![1, 2],
            features: vec!["f".into()],
        }),
        1,
    );
    hostile::<HelloReply>(
        &body_of(&HelloReply {
            version: 1,
            features: vec!["f".into()],
            daemon: Some("d".into()),
        }),
        2,
    );
    hostile::<ErrorReply>(
        &body_of(&ErrorReply {
            code: ErrorCode::BadRequest,
            message: "m".into(),
        }),
        3,
    );
    hostile::<Status>(&body_of(&Status {}), 4);
    hostile::<StatusReply>(
        &body_of(&StatusReply {
            entries: 1,
            pinned: 2,
            history_limit: 3,
            clipboard_id: Some(4),
            primary_id: Some(5),
        }),
        5,
    );
    hostile::<List>(
        &body_of(&List {
            query: Some("q".into()),
            offset: 1,
            limit: Some(2),
            thumbs: true,
        }),
        6,
    );
    hostile::<ListReply>(
        &body_of(&ListReply {
            entries: vec![entry.clone(), entry.clone()],
            total: 2,
        }),
        7,
    );
    hostile::<Restore>(
        &body_of(&Restore {
            id: 1,
            selection: Selection::Both,
            formats: Formats::Mime,
            mime: Some("m".into()),
        }),
        10,
    );
    hostile::<RestoreReply>(&body_of(&RestoreReply {}), 11);
    hostile::<Pin>(&body_of(&Pin { id: 1, pinned: true }), 12);
    hostile::<PinReply>(&body_of(&PinReply {}), 13);
    hostile::<Delete>(&body_of(&Delete { id: 1 }), 14);
    hostile::<DeleteReply>(&body_of(&DeleteReply {}), 15);
    hostile::<Clear>(&body_of(&Clear { include_pinned: true }), 16);
    hostile::<ClearReply>(&body_of(&ClearReply { removed: 1 }), 17);
    hostile::<Subscribe>(&body_of(&Subscribe { thumbs: true }), 18);
    hostile::<SubscribeReply>(&body_of(&SubscribeReply {}), 19);
    hostile::<Added>(&body_of(&Added { entry: entry.clone() }), 20);
    hostile::<Updated>(&body_of(&Updated { entry }), 21);
    hostile::<Removed>(&body_of(&Removed { id: 1 }), 22);
    hostile::<Cleared>(&body_of(&Cleared {}), 23);
    hostile::<SelectionChanged>(
        &body_of(&SelectionChanged {
            selection: EventSelection::Primary,
            id: Some(1),
        }),
        24,
    );
}

// ---- Element caps on decoded sequences (bounded_vec) ----

#[test]
fn sequences_over_their_element_cap_are_errors() {
    // A hostile peer could otherwise fill a 32 MiB frame with 1-byte elements that decode to
    // 24+ bytes each. The caps sit well above anything a real daemon or client sends.
    let features = |n: usize| vec![String::new(); n];
    let hello = |n| {
        decode_body::<Hello>(&body_of(&Hello {
            versions: vec![1],
            features: features(n),
        }))
    };
    assert!(hello(64).is_ok());
    assert!(matches!(hello(65), Err(ProtocolError::Decode(_))));
    let reply = |n| {
        decode_body::<HelloReply>(&body_of(&HelloReply {
            version: 1,
            features: features(n),
            daemon: None,
        }))
    };
    assert!(reply(64).is_ok());
    assert!(matches!(reply(65), Err(ProtocolError::Decode(_))));

    let list = |n: u64| {
        decode_body::<ListReply>(&body_of(&ListReply {
            entries: (0..n).map(sample_entry).collect(),
            total: n,
        }))
    };
    assert!(list(1000).is_ok());
    assert!(matches!(list(1001), Err(ProtocolError::Decode(_))));

    let types = |n: usize| {
        let mut e = sample_entry(1);
        e.types = vec![
            TypeSummary {
                mime: String::new(),
                size: 0
            };
            n
        ];
        decode_body::<Added>(&body_of(&Added { entry: e }))
    };
    assert!(types(256).is_ok());
    assert!(matches!(types(257), Err(ProtocolError::Decode(_))));
}
