//! Frame header wire format and message type mapping.

use clipleaf_ipc::protocol::header::{EventType, HEADER_LEN, Header, MAX_FRAME_LEN, MAX_REQUEST_LEN, MessageType, RequestType};

const ALL_REQUESTS: [RequestType; 8] = [
    RequestType::Hello,
    RequestType::Status,
    RequestType::List,
    RequestType::Restore,
    RequestType::Pin,
    RequestType::Delete,
    RequestType::Clear,
    RequestType::Subscribe,
];

const ALL_EVENTS: [EventType; 5] = [
    EventType::Added,
    EventType::Updated,
    EventType::Removed,
    EventType::Cleared,
    EventType::SelectionChanged,
];

#[test]
fn constants_match_the_spec() {
    assert_eq!(HEADER_LEN, 12);
    assert_eq!(MAX_REQUEST_LEN, 64 * 1024);
    assert_eq!(MAX_FRAME_LEN, 32 * 1024 * 1024);
}

/// Same layout as Python `struct.pack('<IBBBBI', len, category, index, nfds, flags, req_id)`.
#[test]
fn encode_pins_exact_wire_bytes() {
    let h = Header {
        len: 0x0403_0201,
        msg_type: MessageType::Request(RequestType::List),
        nfds: 7,
        flags: 9,
        req_id: 0xDEAD_BEEF,
    };
    assert_eq!(h.encode(), [0x01, 0x02, 0x03, 0x04, 1, 3, 7, 9, 0xEF, 0xBE, 0xAD, 0xDE]);

    let ev = Header {
        len: 1,
        msg_type: MessageType::Event(EventType::SelectionChanged),
        nfds: 0,
        flags: 0,
        req_id: 0,
    };
    assert_eq!(ev.encode(), [1, 0, 0, 0, 4, 5, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn decode_pins_exact_wire_bytes() {
    let h = Header::decode(&[0xFF, 0xFF, 0xFF, 0xFF, 2, 8, 1, 0x80, 0x01, 0x00, 0x00, 0x80]);
    assert_eq!(h.len, u32::MAX);
    assert_eq!(h.msg_type, MessageType::Reply(RequestType::Subscribe));
    assert_eq!(h.nfds, 1);
    assert_eq!(h.flags, 0x80);
    assert_eq!(h.req_id, 0x8000_0001);
}

#[test]
fn header_round_trips_for_every_known_type() {
    let mut types: Vec<MessageType> = Vec::new();
    for r in ALL_REQUESTS {
        types.push(MessageType::Request(r));
        types.push(MessageType::Reply(r));
    }
    types.extend(ALL_EVENTS.map(MessageType::Event));
    types.push(MessageType::Error);

    for (i, t) in types.into_iter().enumerate() {
        let h = Header {
            len: 1000 + i as u32,
            msg_type: t,
            nfds: i as u8,
            flags: 0xFF - i as u8,
            req_id: u32::MAX - i as u32,
        };
        let d = Header::decode(&h.encode());
        assert_eq!(
            (d.len, d.msg_type, d.nfds, d.flags, d.req_id),
            (h.len, h.msg_type, h.nfds, h.flags, h.req_id)
        );
    }
}

#[test]
fn every_request_type_maps_to_its_index_as_request_and_reply() {
    for (i, r) in ALL_REQUESTS.into_iter().enumerate() {
        let index = i as u8 + 1;
        assert_eq!(MessageType::Request(r).to_wire(), [1, index], "{r:?}");
        assert_eq!(MessageType::Reply(r).to_wire(), [2, index], "{r:?}");
        assert_eq!(MessageType::from_wire(1, index), MessageType::Request(r));
        assert_eq!(MessageType::from_wire(2, index), MessageType::Reply(r));
    }
}

#[test]
fn every_event_type_maps_to_its_index() {
    for (i, e) in ALL_EVENTS.into_iter().enumerate() {
        let index = i as u8 + 1;
        assert_eq!(MessageType::Event(e).to_wire(), [4, index], "{e:?}");
        assert_eq!(MessageType::from_wire(4, index), MessageType::Event(e));
    }
}

#[test]
fn error_is_category_3_index_0() {
    assert_eq!(MessageType::Error.to_wire(), [3, 0]);
    assert_eq!(MessageType::from_wire(3, 0), MessageType::Error);
}

#[test]
fn unknown_types_are_unknown_and_keep_their_wire_value() {
    let cases: &[(u8, u8)] = &[
        (0, 0),
        (0, 1),
        (5, 1),
        (255, 1),
        (255, 255),
        (3, 1),
        (3, 255),
        (1, 0),
        (1, 9),
        (1, 255),
        (2, 0),
        (2, 9),
        (2, 255),
        (4, 0),
        (4, 6),
        (4, 255),
    ];
    for &(c, i) in cases {
        let t = MessageType::from_wire(c, i);
        assert_eq!(t, MessageType::Unknown(u16::from_be_bytes([c, i])), "({c}, {i})");
        assert_eq!(t.to_wire(), [c, i], "({c}, {i})");
    }
}

#[test]
fn from_wire_u16_matches_from_wire_for_all_values() {
    for c in 0..=255u8 {
        for i in 0..=255u8 {
            let t = MessageType::from_wire(c, i);
            assert_eq!(MessageType::from_wire_u16(u16::from_be_bytes([c, i])), t, "({c}, {i})");
            // to_wire is the exact inverse for every value, known or not.
            assert_eq!(t.to_wire(), [c, i], "({c}, {i})");
        }
    }
}
