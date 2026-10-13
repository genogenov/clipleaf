//! Byte-level behavior of the connection, observed through the session's public API: messages
//! split around WouldBlock, buffer boundaries, EOF, impossible headers. The peer is the scripted
//! raw peer; this binary never scans fds or spawns processes, so non-blocking EOF checks are valid.

mod helpers;

use std::io::ErrorKind;

use clipleaf_core::WlSessionEvent;
use clipleaf_test_common::raw::wire;
use helpers::*;

#[test]
fn a_non_blocking_read_on_a_silent_connection_returns_none_at_once() {
    // The peer stays open and silent: a blocking read would hang forever.
    let mut rig = rig();
    for _ in 0..3 {
        assert!(rig.session.try_next_event().unwrap().is_none());
    }
}

#[test]
fn an_event_split_across_would_block_is_delivered_once_complete() {
    // Every cut point of a 12-byte event: inside the header (1..8) and inside the body (8..12).
    for cut in 1..12 {
        let mut rig = rig();
        let whole = selection(OFFER_A);
        let mut first = new_offer(OFFER_A, "text/plain");
        first.extend(&whole[..cut]);
        rig.peer().send(&first);
        assert!(rig.session.try_next_event().unwrap().is_none(), "cut {cut}");
        // The partial message survives every WouldBlock.
        assert!(rig.session.try_next_event().unwrap().is_none(), "cut {cut}");

        rig.peer().send(&whole[cut..]);
        assert!(
            matches!(
                rig.session.try_next_event().unwrap(),
                Some(WlSessionEvent::Selection {
                    offer_id: Some(OFFER_A)
                })
            ),
            "cut {cut}"
        );
        assert_eq!(
            rig.session.get_offer().unwrap().offered_mime_types(),
            ["text/plain"]
        );
    }
}

#[test]
fn messages_straddling_the_end_of_the_read_buffer_are_all_delivered_intact() {
    // 8-byte cancelled + 12-byte selection(none) = 20 bytes per pair; 20 does not divide the
    // 4096-byte buffer, so reads end mid-message over and over.
    let mut rig = rig();
    let sources: Vec<u32> = (0..3)
        .map(|_| rig.session.create_data_source().unwrap().local_id)
        .collect();
    rig.flush_and_drain();
    let pairs = 500usize;
    let mut bytes = Vec::new();
    for i in 0..pairs {
        bytes.extend(source_cancelled(sources[i % 3]));
        bytes.extend(selection(0));
    }
    rig.peer().send(&bytes);

    let events = drain_events(&mut rig.session);
    let expected: Vec<_> = (0..pairs)
        .flat_map(|i| [("cancelled", Some(sources[i % 3])), ("selection", None)])
        .collect();
    assert_eq!(kinds(&events), expected);
}

#[test]
fn a_partial_message_at_the_very_end_of_a_full_buffer_waits_for_the_rest() {
    // 341 whole 12-byte messages fill 4092 bytes; 4 bytes of the next one end the buffer
    // exactly, so reading on needs room that only moving the partial bytes frees.
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();
    let whole = 4096 / 12;
    let mut bytes = Vec::new();
    for _ in 0..whole {
        bytes.extend(selection(0));
    }
    let last = source_cancelled(source);
    bytes.extend(&last[..4]);
    rig.peer().send(&bytes);

    assert_eq!(drain_events(&mut rig.session).len(), whole);
    rig.peer().send(&last[4..]);
    assert_eq!(
        kinds(&drain_events(&mut rig.session)),
        [("cancelled", Some(source))]
    );
}

#[test]
fn messages_already_buffered_are_handled_by_the_next_call() {
    let mut rig = rig();
    // `done` and `delete_id` arrive in the same read.
    let mut events = done();
    events.extend(wire::msg(
        DISPLAY,
        DISPLAY_DELETE_ID,
        &CALLBACK.to_ne_bytes(),
    ));
    rig.peer().send(&events);

    assert!(matches!(
        rig.session.next_event().unwrap(),
        WlSessionEvent::SyncDone
    ));
    // The socket has nothing left, but the delete_id is in the buffer and must still count:
    // the callback's id is free again.
    assert!(rig.session.try_next_event().unwrap().is_none());
    assert_eq!(rig.session.create_data_source().unwrap().local_id, CALLBACK);
}

#[test]
fn a_closed_connection_is_unexpected_eof_in_both_modes() {
    for blocking in [true, false] {
        let mut rig = rig();
        // A whole event first: it is still delivered, and only then comes the EOF.
        let mut events = new_offer(OFFER_A, "text/plain");
        events.extend(selection(OFFER_A));
        rig.peer().send(&events);
        rig.peer().shutdown_write();

        assert!(rig.session.try_next_event().unwrap().is_some());
        let mut next = || {
            if blocking {
                err_of(rig.session.next_event())
            } else {
                rig.session.try_next_event().unwrap_err()
            }
        };
        // A hang-up is an error every time, never "nothing yet".
        for _ in 0..2 {
            let err = next();
            assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
            assert!(err.to_string().contains("Compositor closed"), "{err}");
        }
    }
}

#[test]
fn eof_in_the_middle_of_a_message_is_a_different_unexpected_eof() {
    for blocking in [true, false] {
        for cut in [3, 8, 11] {
            let mut rig = rig();
            let whole = selection(OFFER_A);
            rig.peer().send(&whole[..cut]);
            rig.peer().shutdown_write();

            let err = if blocking {
                err_of(rig.session.next_event())
            } else {
                rig.session.try_next_event().unwrap_err()
            };
            assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "cut {cut}: {err}");
            assert!(
                err.to_string().contains("partial message"),
                "cut {cut}: {err}"
            );
        }
    }
}

#[test]
fn a_header_with_an_impossible_size_is_an_error() {
    // Under 8 would never make progress; over the buffer could never be completed.
    for size in [4u32, 4097] {
        let mut rig = rig();
        let mut header = SEAT.to_ne_bytes().to_vec();
        header.extend((size << 16).to_ne_bytes());
        rig.peer().send(&header);

        let err = err_of(rig.session.next_event());
        assert!(err.to_string().contains("invalid"), "size {size}: {err}");
    }
}
