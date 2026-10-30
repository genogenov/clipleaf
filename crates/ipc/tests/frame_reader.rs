//! FrameReader: framing over arbitrary read boundaries, buffer growth, and the length cap.

mod common;

use std::time::{Duration, Instant};

use clipleaf_ipc::{
    frame::frame_reader::FrameReader,
    protocol::{
        error::ProtocolError,
        header::{EventType, HEADER_LEN, Header, MAX_FRAME_LEN, MessageType, RequestType},
    },
};
use common::{ChunkReader, Frame, XorShift, raw_frame, read_all_frames};

/// (type, req_id, body)
type Expected = (MessageType, u32, Vec<u8>);

const STATUS_REQ: [u8; 2] = [1, 2];
const LIST_REPLY: [u8; 2] = [2, 3];
const ADDED: [u8; 2] = [4, 1];

fn three_frames() -> (Vec<u8>, Vec<Expected>) {
    let parts = vec![
        (STATUS_REQ, 7u32, vec![0x80]),
        (ADDED, 0, (0..40u8).collect::<Vec<_>>()),
        (LIST_REPLY, 8, vec![]),
    ];
    let mut wire = Vec::new();
    let mut expected = Vec::new();
    for (t, id, body) in parts {
        wire.extend(raw_frame(t, id, &body));
        expected.push((MessageType::from_wire(t[0], t[1]), id, body));
    }
    (wire, expected)
}

fn check(frames: &[Frame], expected: &[Expected]) {
    assert_eq!(frames.len(), expected.len());
    for (f, (t, id, body)) in frames.iter().zip(expected) {
        assert_eq!(f.msg_type, *t);
        assert_eq!(f.req_id, *id);
        assert_eq!(f.len as usize, body.len());
        assert_eq!(&f.body, body);
    }
}

#[test]
fn one_frame() {
    let wire = raw_frame(STATUS_REQ, 42, &[0x80]);
    let mut r = FrameReader::new(4096, 1 << 20);
    let mut src = ChunkReader::new(wire, vec![]);
    let frames = read_all_frames(&mut r, &mut src);
    check(&frames, &[(MessageType::Request(RequestType::Status), 42, vec![0x80])]);
}

#[test]
fn nfds_and_flags_are_passed_through() {
    let h = Header {
        len: 2,
        msg_type: MessageType::Event(EventType::Removed),
        nfds: 3,
        flags: 0xA5,
        req_id: 0,
    };
    let mut wire = h.encode().to_vec();
    wire.extend([0xAB, 0xCD]);
    let mut r = FrameReader::new(0, 0);
    let frames = read_all_frames(&mut r, &mut ChunkReader::new(wire, vec![]));
    assert_eq!(frames.len(), 1);
    assert_eq!((frames[0].nfds, frames[0].flags), (3, 0xA5));
    assert_eq!(frames[0].body, [0xAB, 0xCD]);
}

#[test]
fn several_frames_in_one_read() {
    let (wire, expected) = three_frames();
    let mut r = FrameReader::new(4096, 1 << 20);
    let mut src = ChunkReader::new(wire, vec![usize::MAX]);
    // A single fill_from must make all three frames available.
    assert!(r.fill_from(&mut src).unwrap() > 0);
    assert!(src.exhausted());
    let mut frames = Vec::new();
    while let Some(res) = r.next_frame() {
        let (h, body) = res.unwrap();
        frames.push(Frame::from_parts(&h, body));
    }
    check(&frames, &expected);
    assert_eq!(r.fill_from(&mut src).unwrap(), 0);
}

#[test]
fn no_frame_until_header_and_body_are_complete() {
    let wire = raw_frame(STATUS_REQ, 1, &[1, 2, 3]);
    let mut r = FrameReader::new(0, 0);
    let mut src = ChunkReader::new(wire.clone(), vec![1; wire.len()]);
    for _ in 0..wire.len() - 1 {
        assert_eq!(r.fill_from(&mut src).unwrap(), 1);
        assert!(r.next_frame().is_none());
    }
    assert_eq!(r.fill_from(&mut src).unwrap(), 1);
    let (h, body) = r.next_frame().unwrap().unwrap();
    assert_eq!((h.req_id, body), (1, &[1u8, 2, 3][..]));
    assert!(r.next_frame().is_none());
}

#[test]
fn split_at_every_byte_offset() {
    let (wire, expected) = three_frames();
    for at in 0..=wire.len() {
        let mut r = FrameReader::new(0, 0);
        let mut src = ChunkReader::new(wire.clone(), vec![at]);
        let frames = read_all_frames(&mut r, &mut src);
        check(&frames, &expected);
    }
}

#[test]
fn split_at_every_pair_of_offsets() {
    let (wire, expected) = three_frames();
    for a in 0..=wire.len() {
        for b in 0..=wire.len() - a {
            let mut r = FrameReader::new(0, 0);
            let mut src = ChunkReader::new(wire.clone(), vec![a, b]);
            check(&read_all_frames(&mut r, &mut src), &expected);
        }
    }
}

#[test]
fn one_byte_reads() {
    let (wire, expected) = three_frames();
    let mut r = FrameReader::new(0, 0);
    let mut src = ChunkReader::new(wire.clone(), vec![1; wire.len()]);
    check(&read_all_frames(&mut r, &mut src), &expected);
}

#[test]
fn large_frame_then_small_frames() {
    let mut rng = XorShift::new(0x1234);
    let big = rng.bytes(1 << 20);
    let mut wire = raw_frame(LIST_REPLY, 5, &[1]);
    wire.extend(raw_frame(LIST_REPLY, 6, &big));
    let (small, small_expected) = three_frames();
    wire.extend(&small);
    wire.extend(raw_frame(LIST_REPLY, 9, &big[..5000]));
    wire.extend(&small);

    let mut expected = vec![(MessageType::Reply(RequestType::List), 5, vec![1])];
    expected.push((MessageType::Reply(RequestType::List), 6, big.clone()));
    expected.extend(small_expected.clone());
    expected.push((MessageType::Reply(RequestType::List), 9, big[..5000].to_vec()));
    expected.extend(small_expected);

    // Socket-like chunk sizes, default buffer far below the large frame.
    for chunk in [1usize, 7, 4096, 65536, usize::MAX] {
        let chunks = if chunk == 1 {
            vec![1; 64]
        } else {
            vec![chunk; wire.len() / chunk.min(wire.len()) + 1]
        };
        let mut r = FrameReader::new(4096, MAX_FRAME_LEN as usize);
        let mut src = ChunkReader::new(wire.clone(), chunks);
        check(&read_all_frames(&mut r, &mut src), &expected);
    }
}

#[test]
fn declared_len_over_cap_is_frame_too_big() {
    for len in [u32::MAX, u32::MAX - 1, 1 << 31, MAX_FRAME_LEN, MAX_FRAME_LEN - HEADER_LEN as u32 + 1] {
        let mut wire = (len).to_le_bytes().to_vec();
        wire.extend([2, 3, 0, 0, 1, 0, 0, 0]);
        let mut r = FrameReader::new(4096, MAX_FRAME_LEN as usize);
        let mut src = ChunkReader::new(wire, vec![]);
        let start = Instant::now();
        assert_eq!(r.fill_from(&mut src).unwrap(), HEADER_LEN);
        assert!(matches!(r.next_frame(), Some(Err(ProtocolError::FrameTooBig))), "len {len}");
        // Stays an error (the stream cannot resync).
        assert!(matches!(r.next_frame(), Some(Err(ProtocolError::FrameTooBig))), "len {len}");
        assert!(start.elapsed() < Duration::from_millis(500), "len {len}: {:?}", start.elapsed());
    }
}

#[test]
fn frame_at_max_frame_len_is_accepted_header_only() {
    // Body exactly MAX_FRAME_LEN - HEADER_LEN: no error, just waits for the body.
    let len = MAX_FRAME_LEN - HEADER_LEN as u32;
    let mut wire = len.to_le_bytes().to_vec();
    wire.extend([2, 3, 0, 0, 1, 0, 0, 0]);
    let mut r = FrameReader::new(4096, MAX_FRAME_LEN as usize);
    let mut src = ChunkReader::new(wire, vec![]);
    r.fill_from(&mut src).unwrap();
    assert!(r.next_frame().is_none());
}

#[test]
fn cap_boundary_with_full_body() {
    let max = 8192usize;
    let ok_body = vec![0x5A; max - HEADER_LEN];
    let mut r = FrameReader::new(0, max);
    let mut src = ChunkReader::new(raw_frame(LIST_REPLY, 3, &ok_body), vec![]);
    let frames = read_all_frames(&mut r, &mut src);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].body, ok_body);

    let over = vec![0x5A; max - HEADER_LEN + 1];
    let mut r = FrameReader::new(0, max);
    let mut src = ChunkReader::new(raw_frame(LIST_REPLY, 3, &over), vec![]);
    r.fill_from(&mut src).unwrap();
    assert!(matches!(r.next_frame(), Some(Err(ProtocolError::FrameTooBig))));
}

#[test]
fn fill_from_returns_zero_at_eof() {
    let mut r = FrameReader::new(0, 0);
    let mut empty = ChunkReader::new(vec![], vec![]);
    assert_eq!(r.fill_from(&mut empty).unwrap(), 0);
    assert!(r.next_frame().is_none());

    // EOF in the middle of a frame: the partial frame is never returned.
    let wire = raw_frame(STATUS_REQ, 1, &[1, 2, 3]);
    let mut src = ChunkReader::new(wire[..wire.len() - 1].to_vec(), vec![]);
    assert_eq!(r.fill_from(&mut src).unwrap(), wire.len() - 1);
    assert!(r.next_frame().is_none());
    assert_eq!(r.fill_from(&mut src).unwrap(), 0);
    assert!(r.next_frame().is_none());
}

#[test]
fn fill_from_propagates_read_errors() {
    struct Failing;
    impl std::io::Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "boom"))
        }
    }
    let mut r = FrameReader::new(0, 0);
    assert_eq!(r.fill_from(&mut Failing).unwrap_err().kind(), std::io::ErrorKind::ConnectionReset);
}

#[test]
fn random_chunking_of_random_frames() {
    let mut rng = XorShift::new(0xC0FFEE);
    for _ in 0..50 {
        let mut wire = Vec::new();
        let mut expected = Vec::new();
        for i in 0..20 {
            let len = match rng.below(10) {
                0 => rng.below(20_000),
                _ => rng.below(300),
            };
            let body = rng.bytes(len);
            wire.extend(raw_frame(ADDED, i, &body));
            expected.push((MessageType::Event(EventType::Added), i, body));
        }
        let chunks: Vec<usize> = (0..400).map(|_| 1 + rng.below(3000)).collect();
        let mut r = FrameReader::new(rng.below(5000), MAX_FRAME_LEN as usize);
        check(&read_all_frames(&mut r, &mut ChunkReader::new(wire, chunks)), &expected);
    }
}

/// Once the header is in, the buffer grows to the whole declared frame in one step, so a large
/// frame takes one more read, not one read per buffer's worth (or per byte).
#[test]
fn large_frame_needs_only_one_more_read_after_its_header() {
    let body = XorShift::new(7).bytes(1 << 20);
    let wire = raw_frame(LIST_REPLY, 1, &body);
    let mut r = FrameReader::new(4096, MAX_FRAME_LEN as usize);
    // unlimited chunks: each read fills whatever buffer space the reader offers
    let mut src = ChunkReader::new(wire, vec![usize::MAX; 1 << 21]);
    let mut reads = 0;
    let got = loop {
        if let Some(res) = r.next_frame() {
            break res.unwrap().1.to_vec();
        }
        assert!(r.fill_from(&mut src).unwrap() > 0, "EOF before the frame completed");
        reads += 1;
    };
    assert_eq!(got, body);
    assert_eq!(reads, 2, "header read + one read for the rest");
}
