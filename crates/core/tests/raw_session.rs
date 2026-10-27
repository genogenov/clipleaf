//! The session's public API against the scripted raw peer: the handshake bytes, event order,
//! fds per `recvmsg`, raw mime bytes, display errors, and one representative malformed case per
//! parser. No relay here, so non-blocking EOF checks are valid (nothing scans fds or spawns).

mod helpers;

use std::{io::ErrorKind, io::Write};

use clipleaf_core::{WlMessageReader, WlSessionEvent, pick_best_mime};
use clipleaf_test_common::{
    fds,
    raw::{self, wire},
};
use helpers::*;

#[test]
fn initialize_binds_the_manager_and_the_seat_and_creates_a_device() {
    // Server versions 9 (seat) and 1 (manager) are clamped to what is implemented: 1 for both.
    let globals = [(1, "wl_seat", 9), (7, "wl_output", 4), (54, "ext_data_control_manager_v1", 1)];
    let mut rig = try_rig(&globals).unwrap();
    let requests = rig.flush_and_drain();

    let strip = |r: &wire::Request| (r.object_id, r.opcode);
    assert_eq!(strip(&requests[0]), (DISPLAY, DISPLAY_GET_REGISTRY));
    assert_eq!(requests[0].u32_at(0), REGISTRY);
    assert_eq!(strip(&requests[1]), (DISPLAY, DISPLAY_SYNC));
    assert_eq!(requests[1].u32_at(0), CALLBACK);

    for (request, (name, interface, new_id)) in requests[2..4]
        .iter()
        .zip([(54, &b"ext_data_control_manager_v1"[..], MANAGER), (1, b"wl_seat", SEAT)])
    {
        assert_eq!(strip(request), (REGISTRY, REGISTRY_BIND));
        let (text, next) = request.string_at(4);
        assert_eq!(request.u32_at(0), name);
        assert_eq!(text, interface);
        assert_eq!((request.u32_at(next), request.u32_at(next + 4)), (1, new_id));
    }

    assert_eq!(strip(&requests[4]), (MANAGER, MANAGER_GET_DATA_DEVICE));
    assert_eq!((requests[4].u32_at(0), requests[4].u32_at(4)), (DEVICE, SEAT));
    assert_eq!(requests.len(), 5);
}

#[test]
fn initialize_fails_without_ext_data_control() {
    let Err(err) = try_rig(&[(1, "wl_seat", 9)]) else {
        panic!("initialized without ext_data_control_manager_v1");
    };
    assert!(err.to_string().contains("ext_data_control_manager_v1"), "{err}");
}

#[test]
fn initialize_fails_without_a_seat() {
    let Err(err) = try_rig(&[(54, "ext_data_control_manager_v1", 1)]) else {
        panic!("initialized without wl_seat");
    };
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn initialize_reports_a_compositor_that_closed_the_connection_as_a_broken_pipe() {
    // The peer is gone before the first request: the write of get_registry + sync fails.
    let comp = raw::RawCompositor::new();
    let stream = clipleaf_core::WlBufferedStream::connect(comp.path()).unwrap();
    comp.accept().close();
    let err = err_of(clipleaf_core::WlSessionManager::initialize(stream));
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err}");
}

#[test]
fn initialize_reports_a_compositor_that_hangs_up_before_the_sync_as_an_unexpected_eof() {
    // The peer accepts the requests but never answers and stops sending: the read hits EOF.
    let comp = raw::RawCompositor::new();
    let stream = clipleaf_core::WlBufferedStream::connect(comp.path()).unwrap();
    let peer = comp.accept();
    peer.shutdown_write();
    let err = err_of(clipleaf_core::WlSessionManager::initialize(stream));
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
    assert!(err.to_string().contains("Compositor closed"), "{err}");
}

#[test]
fn events_are_delivered_one_at_a_time_in_order() {
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();
    let (mut payload, fd) = fds::channel();

    let mut before_send = new_offer(OFFER_A, "text/plain");
    before_send.extend(selection(OFFER_A));
    before_send.extend(new_offer(OFFER_B, "image/png"));
    before_send.extend(primary_selection(OFFER_B));
    rig.peer().send(&before_send);
    rig.peer()
        .send_with_fds(&source_send(source, b"text/html"), &[std::os::fd::AsFd::as_fd(&fd)]);
    drop(fd);
    let mut after_send = source_cancelled(source);
    after_send.extend(done(CALLBACK));
    rig.peer().send(&after_send);

    // Everything is already readable, yet each call handles only up to its own event.
    let WlSessionEvent::Selection { offer_id } = rig.session.next_event().unwrap() else {
        panic!("expected the selection first");
    };
    assert_eq!(offer_id, Some(OFFER_A));
    assert_eq!(rig.session.get_offer().unwrap().id(), OFFER_A);
    assert!(rig.session.get_primary_offer().is_none(), "not read yet");

    let WlSessionEvent::PrimarySelection { offer_id } = rig.session.next_event().unwrap() else {
        panic!("expected the primary selection second");
    };
    assert_eq!(offer_id, Some(OFFER_B));
    assert_eq!(rig.session.get_primary_offer().unwrap().id(), OFFER_B);

    let WlSessionEvent::DataControlSend { source: from, mime, fd } = rig.session.next_event().unwrap() else {
        panic!("expected the send third");
    };
    assert_eq!((from, &*mime), (source, &b"text/html"[..]));
    std::fs::File::from(fd).write_all(b"served").unwrap();
    assert_eq!(fds::read_to_eof_now(&mut payload), b"served");

    assert!(matches!(
        rig.session.next_event().unwrap(),
        WlSessionEvent::DataControlCancelled { source: s } if s == source
    ));
    assert!(matches!(
        rig.session.next_event().unwrap(),
        WlSessionEvent::SyncDone { callback_id } if callback_id == CALLBACK
    ));
    assert!(rig.session.try_next_event().unwrap().is_none());
}

#[test]
fn try_next_event_never_flushes_and_one_flush_sends_everything_queued() {
    // initialize sent get_registry + sync (it waited for the reply); the 2 binds and
    // get_data_device are queued. The peer stays silent.
    let mut rig = try_rig(&GLOBALS).unwrap();
    assert_eq!(rig.peer().requests().len(), 2);
    for _ in 0..3 {
        assert!(rig.session.try_next_event().unwrap().is_none());
    }
    // A poll loop drains first and flushes once: nothing went out while draining.
    assert!(rig.peer().recv_batches().is_empty());
    assert!(rig.session.has_pending_writes());
    assert_eq!(rig.flush_and_drain().len(), 3);
    assert!(!rig.session.has_pending_writes());
}

#[test]
fn a_burst_is_fully_drained_by_repeated_try_next_event() {
    let mut rig = rig();
    let mut burst = Vec::new();
    for id in [OFFER_A, OFFER_B, OFFER_C] {
        burst.extend(new_offer(id, "text/plain"));
        burst.extend(selection(id));
    }
    rig.peer().send(&burst);

    let events = drain_events(&mut rig.session);
    assert_eq!(
        kinds(&events),
        [("selection", Some(OFFER_A)), ("selection", Some(OFFER_B)), ("selection", Some(OFFER_C))]
    );
    // Drained, and nothing more however often it is asked.
    assert!(rig.session.try_next_event().unwrap().is_none());
    assert!(rig.session.try_next_event().unwrap().is_none());
    assert_eq!(destroyed_offers(&rig.flush_and_drain()), [OFFER_A, OFFER_B]);
}

#[test]
fn a_display_error_is_an_error_with_its_message() {
    for blocking in [true, false] {
        let mut rig = rig();
        let mut body = DEVICE.to_ne_bytes().to_vec();
        body.extend(1u32.to_ne_bytes());
        body.extend(wire::string(b"invalid arguments"));
        rig.peer().send(&wire::msg(DISPLAY, DISPLAY_ERROR, &body));

        let err = if blocking {
            err_of(rig.session.next_event())
        } else {
            rig.session.try_next_event().unwrap_err()
        };
        assert!(err.to_string().contains("invalid arguments"), "{err}");
        assert!(err.to_string().contains("error_code=1"), "{err}");
    }
}

#[test]
fn malformed_or_unexpected_events_end_in_an_error_that_consumes_only_their_own_message() {
    let mut rig = rig();
    let cases = [
        ("unknown opcode", wire::msg(DEVICE, 9, &[])),
        ("missing argument", wire::msg(DEVICE, DEVICE_SELECTION, &[])),
        ("new_id outside the server range", device_event(DEVICE_DATA_OFFER, 5)),
        ("finished", wire::msg(DEVICE, DEVICE_FINISHED, &[])),
        ("object we never created", wire::msg(12, 0, &[])),
        ("callback with an unknown opcode", wire::msg(CALLBACK, 7, &[])),
    ];
    rig.peer().send(&cases.iter().flat_map(|(_, m)| m.clone()).collect::<Vec<_>>());
    rig.peer().shutdown_write();

    // Each error consumes only its own message, so the next call reports the next case.
    for (case, _) in cases {
        let err = err_of(rig.session.next_event());
        assert_ne!(err.kind(), ErrorKind::UnexpectedEof, "{case}: {err}");
    }
    let err = err_of(rig.session.next_event());
    assert_eq!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
}

#[test]
fn a_send_event_hands_over_raw_mime_bytes_and_its_fd() {
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();
    // Not UTF-8: consumers only compare it with their own types, so it is just unknown.
    let raw_mime: &[u8] = &[b't', 0xff, b'/', 0xfe, 0x80];
    let (mut payload, fd) = fds::channel();
    rig.peer().send_with_fds(&source_send(source, raw_mime), &[std::os::fd::AsFd::as_fd(&fd)]);
    drop(fd);

    let WlSessionEvent::DataControlSend { source: from, mime, fd } = rig.session.next_event().unwrap() else {
        panic!("expected a send");
    };
    assert_eq!(from, source);
    assert_eq!(&*mime, raw_mime);
    std::fs::File::from(fd).write_all(b"ok").unwrap();
    assert_eq!(fds::read_to_eof_now(&mut payload), b"ok");
}

#[test]
fn a_send_event_without_an_fd_is_an_error() {
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();
    rig.peer().send(&source_send(source, b"text/plain"));
    let err = err_of(rig.session.next_event());
    assert!(err.to_string().contains("file descriptor"), "{err}");
}

#[test]
fn fds_of_two_sends_are_held_together_and_stay_paired_with_their_messages() {
    // libwayland may attach 28 fds to a sendmsg whose bytes end mid-message. The client then has
    // to hold those 28 fds, read again, and take 28 more before it can pop any: 56 at once.
    // Batch 1 carries 28 fds and only the first 4 bytes; batch 2 the rest and 28 more fds.
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();

    let mut bytes = Vec::new();
    let mut readers = Vec::new();
    let mut write_ends = Vec::new();
    for i in 0..56u8 {
        let (reader, writer) = fds::channel();
        readers.push(reader);
        write_ends.push(writer);
        bytes.extend(source_send(source, &[b'm', i]));
    }
    let (first, second) = write_ends.split_at(28);
    fn borrow(fds: &[std::os::fd::OwnedFd]) -> Vec<std::os::fd::BorrowedFd<'_>> {
        fds.iter().map(std::os::fd::AsFd::as_fd).collect()
    }
    rig.peer().send_with_fds(&bytes[..4], &borrow(first));
    rig.peer().send_with_fds(&bytes[4..], &borrow(second));
    drop(write_ends);

    for (index, reader) in readers.iter_mut().enumerate() {
        let WlSessionEvent::DataControlSend { mime, fd, .. } = rig.session.next_event().unwrap() else {
            panic!("expected send {index}");
        };
        assert_eq!(&*mime, [b'm', index as u8], "message {index}");
        std::fs::File::from(fd).write_all(&[index as u8]).unwrap();
        assert_eq!(fds::read_to_eof_now(reader), [index as u8], "fd {index}");
    }
}

#[test]
fn more_than_28_fds_in_one_send_is_an_error_and_leaks_none() {
    // A compositor that violates the 28-fd limit: the kernel truncates the control data.
    // The session must end with a clean error, and every fd, those that arrived and the one
    // the kernel dropped, must be closed.
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap().local_id;
    rig.flush_and_drain();

    let mut bytes = Vec::new();
    let mut readers = Vec::new();
    let mut write_ends = Vec::new();
    for i in 0..29u8 {
        let (reader, writer) = fds::channel();
        readers.push(reader);
        write_ends.push(writer);
        bytes.extend(source_send(source, &[b'm', i]));
    }
    let borrowed: Vec<_> = write_ends.iter().map(std::os::fd::AsFd::as_fd).collect();
    rig.peer().send_with_fds(&bytes, &borrowed);
    drop(write_ends);

    let err = err_of(rig.session.next_event());
    assert!(err.to_string().contains("truncated"), "{err}");
    // The 28 fds that did arrive sit in the session's buffer; dropping it closes them.
    drop(rig);
    for (index, reader) in readers.iter_mut().enumerate() {
        assert_eq!(fds::read_to_eof_now(reader), b"", "fd {index} leaked");
    }
}

#[test]
fn a_replaced_offer_is_destroyed_and_the_destroy_reaches_the_compositor() {
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/plain");
    events.extend(selection(OFFER_A));
    events.extend(new_offer(OFFER_B, "text/plain"));
    events.extend(selection(OFFER_B));
    rig.peer().send(&events);

    rig.session.next_event().unwrap();
    rig.session.next_event().unwrap();
    // Back in the loop: drained, then the loop's one flush sends the queued destroy.
    assert!(rig.session.try_next_event().unwrap().is_none());

    let requests = rig.flush_and_drain();
    assert_eq!(
        requests,
        [wire::Request {
            object_id: OFFER_A,
            opcode: OFFER_DESTROY,
            body: Vec::new()
        }]
    );
    assert_eq!(rig.session.get_offer().unwrap().id(), OFFER_B);
}

#[test]
fn clearing_the_selection_destroys_the_previous_offer() {
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/plain");
    events.extend(selection(OFFER_A));
    events.extend(selection(0));
    rig.peer().send(&events);

    let events = drain_events(&mut rig.session);
    assert_eq!(kinds(&events), [("selection", Some(OFFER_A)), ("selection", None)]);
    assert!(rig.session.get_offer().is_none());
    assert_eq!(destroyed_offers(&rig.flush_and_drain()), [OFFER_A]);
}

#[test]
fn selection_churn_destroys_each_previous_offer_and_never_runs_out_of_slots() {
    let mut rig = rig();
    // The compositor reuses an id once it is destroyed, so two ids are enough.
    let ids_in_use = [OFFER_A, OFFER_B];
    // More rounds than the router (16) and the offer tracker (8) have slots.
    let rounds = 40;
    let mut events = Vec::new();
    for round in 0..rounds {
        let id = ids_in_use[round % 2];
        events.extend(new_offer(id, "text/plain"));
        events.extend(primary_selection(id));
    }
    rig.peer().send(&events);

    assert_eq!(drain_events(&mut rig.session).len(), rounds);
    let expected: Vec<u32> = (0..rounds - 1).map(|round| ids_in_use[round % 2]).collect();
    // Nothing went out while draining; the loop's one flush sends all 39 destroys.
    assert!(rig.peer().recv_batches().is_empty());
    assert_eq!(destroyed_offers(&rig.flush_and_drain()), expected);
}

#[test]
fn events_for_an_offer_we_already_destroyed_are_ignored() {
    // Protocol-legal: the compositor may still have events in flight for an offer we destroyed.
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/plain");
    events.extend(selection(OFFER_A));
    events.extend(new_offer(OFFER_B, "text/plain"));
    events.extend(selection(OFFER_B)); // destroys A
    events.extend(mime_offer(OFFER_A, "text/late")); // late event for A
    events.extend(selection(0));
    rig.peer().send(&events);

    let events = drain_events(&mut rig.session);
    assert_eq!(
        kinds(&events),
        [("selection", Some(OFFER_A)), ("selection", Some(OFFER_B)), ("selection", None)]
    );
}

#[test]
fn a_burst_right_after_bind_is_routed_per_offer_and_tracks_both_selections() {
    let mut rig = rig();
    // What a compositor sends right after bind + get_data_device + sync, in one write.
    let mut burst = wire::msg(SEAT, 0, &3u32.to_ne_bytes()); // wl_seat.capabilities
    burst.extend(device_event(DEVICE_DATA_OFFER, OFFER_A));
    burst.extend(mime_offer(OFFER_A, "text/html"));
    burst.extend(mime_offer(OFFER_A, "text/plain;charset=utf-8"));
    burst.extend(selection(OFFER_A));
    burst.extend(new_offer(OFFER_B, "text/plain"));
    burst.extend(primary_selection(OFFER_B));
    burst.extend(done(CALLBACK));
    rig.peer().send(&burst);
    rig.peer().shutdown_write();

    let mut events = Vec::new();
    loop {
        let event = rig.session.next_event().unwrap();
        let finished = matches!(event, WlSessionEvent::SyncDone { callback_id } if callback_id == CALLBACK);
        events.push(event);
        if finished {
            break;
        }
    }
    assert_eq!(
        kinds(&events),
        [("selection", Some(OFFER_A)), ("primary", Some(OFFER_B)), ("sync", Some(CALLBACK))]
    );

    let offer = rig.session.get_offer().unwrap();
    assert_eq!(offer.id(), OFFER_A);
    assert_eq!(offer.offered_mime_types(), ["text/html", "text/plain;charset=utf-8"]);
    assert_eq!(
        pick_best_mime(offer.offered_mime_types()).map(|m| m.mime().to_string()).as_deref(),
        Some("text/plain;charset=utf-8")
    );
    let primary = rig.session.get_primary_offer().unwrap();
    assert_eq!(primary.id(), OFFER_B);
    assert_eq!(primary.offered_mime_types(), ["text/plain"]);
}

#[test]
fn receive_offer_asks_the_compositor_for_the_preferred_type_with_the_fd() {
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/html");
    events.extend(mime_offer(OFFER_A, "text/plain;charset=utf-8"));
    events.extend(selection(OFFER_A));
    rig.peer().send(&events);
    rig.session.next_event().unwrap();

    let (mut payload, fd) = fds::channel();
    let asked = rig.session.receive_offer(false, None, fd).unwrap().map(|m| m.to_string());
    assert_eq!(asked.as_deref(), Some("text/plain;charset=utf-8"));

    rig.session.flush().unwrap();
    let mut batches = rig.peer().recv_batches();
    assert_eq!(batches.iter().map(|b| b.fds.len()).sum::<usize>(), 1);
    let fd = batches.iter_mut().find_map(|b| b.fds.pop()).unwrap();
    let receive = wire::parse(&raw::all_bytes(&batches)).pop().unwrap();
    assert_eq!((receive.object_id, receive.opcode), (OFFER_A, OFFER_RECEIVE));
    assert_eq!(WlMessageReader::new(&receive.body).str(), Some(&b"text/plain;charset=utf-8"[..]));
    std::fs::File::from(fd).write_all(b"data").unwrap();
    assert_eq!(fds::read_to_eof_now(&mut payload), b"data");
}

#[test]
fn receive_offer_with_a_type_that_is_not_offered_asks_for_nothing_and_with_no_offer_fails() {
    let mut rig = rig();
    let (mut payload, fd) = fds::channel();
    let err = err_of(rig.session.receive_offer(false, None, fd));
    assert!(err.to_string().contains("nothing found to paste"), "{err}");
    // The fd was not leaked on the way out.
    assert_eq!(fds::read_to_eof_now(&mut payload), b"");

    let mut events = new_offer(OFFER_A, "text/plain");
    events.extend(selection(OFFER_A));
    rig.peer().send(&events);
    rig.session.next_event().unwrap();
    let (mut payload, fd) = fds::channel();
    assert!(rig.session.receive_offer(false, Some("image/png"), fd).unwrap().is_none());
    assert_eq!(fds::read_to_eof_now(&mut payload), b"");
    assert!(rig.flush_and_drain().is_empty(), "no receive was sent");
}

#[test]
fn mime_types_are_filtered_deduplicated_and_capped() {
    let mut rig = rig();
    let mut events = device_event(DEVICE_DATA_OFFER, OFFER_A);
    // Order kept, duplicates dropped, invalid types (control characters, empty, non-UTF-8) skipped.
    for mime in [&b"image/webp"[..], b"text/plain", b"image/webp", b"text/\x1b[31m", b"", b"text/\xff"] {
        events.extend(wire::msg(OFFER_A, OFFER_OFFER, &wire::string(mime)));
    }
    // More than the cap of 128: the rest is skipped.
    for i in 0..140 {
        events.extend(mime_offer(OFFER_A, &format!("application/x-{i}")));
    }
    events.extend(selection(OFFER_A));
    rig.peer().send(&events);
    rig.session.next_event().unwrap();

    let types = rig.session.get_offer().unwrap().offered_mime_types();
    assert_eq!(types.len(), 128);
    assert_eq!(types[..2], ["image/webp", "text/plain"]);
    // Known types are the static strings; others are allocated.
    assert!(matches!(types[0], std::borrow::Cow::Owned(_)));
    assert!(matches!(types[1], std::borrow::Cow::Borrowed(_)));
    assert_eq!(types[2], "application/x-0");
    assert_eq!(types[127], "application/x-125");
}

#[test]
fn the_preferred_type_is_the_best_known_text_then_other_text_then_the_first() {
    let preferred = |mimes: &[&str]| {
        let mut rig = rig();
        let mut events = device_event(DEVICE_DATA_OFFER, OFFER_A);
        for m in mimes {
            events.extend(mime_offer(OFFER_A, m));
        }
        events.extend(selection(OFFER_A));
        rig.peer().send(&events);
        rig.session.next_event().unwrap();
        pick_best_mime(rig.session.get_offer().unwrap().offered_mime_types()).map(|m| m.mime().to_string())
    };
    assert_eq!(
        preferred(&["text/html", "STRING", "UTF8_STRING", "image/png"]).as_deref(),
        Some("UTF8_STRING")
    );
    assert_eq!(preferred(&["image/png", "text/uri-list"]).as_deref(), Some("text/uri-list"));
    assert_eq!(preferred(&["image/png", "image/jpeg"]).as_deref(), Some("image/png"));
    assert_eq!(preferred(&["text/plain;charset=utf-16", "text/html"]).as_deref(), Some("text/html"));
    assert_eq!(
        preferred(&["text/plain;charset=utf-16", "image/png"]).as_deref(),
        Some("text/plain;charset=utf-16")
    );
    assert_eq!(preferred(&[]), None);
}

#[test]
fn a_requested_type_must_match_exactly() {
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/plain;charset=utf-8");
    events.extend(mime_offer(OFFER_A, "image/png"));
    events.extend(selection(OFFER_A));
    rig.peer().send(&events);
    rig.session.next_event().unwrap();
    let offer = rig.session.get_offer().unwrap();
    assert_eq!(offer.asked_mime_type("image/png").map(|m| m.as_ref()), Some("image/png"));
    for missing in ["image", "text/plain", "IMAGE/PNG"] {
        assert!(offer.asked_mime_type(missing).is_none(), "{missing}");
    }
}

#[test]
fn object_ids_are_dense_and_reused_after_delete_id() {
    let mut rig = rig();
    // Dense: the next free client ids after the device (6).
    let a = rig.session.create_data_source().unwrap().local_id;
    let b = rig.session.create_data_source().unwrap().local_id;
    assert_eq!((a, b), (7, 8));
    // Reused after the compositor confirms the delete.
    rig.peer().send(&wire::msg(DISPLAY, DISPLAY_DELETE_ID, &a.to_ne_bytes()));
    assert!(rig.session.try_next_event().unwrap().is_none());
    assert_eq!(rig.session.create_data_source().unwrap().local_id, a);
}

#[test]
fn offers_anywhere_in_the_server_range_up_to_the_cap_are_tracked() {
    // The server table grows on demand: the compositor picks the ids, not us.
    for id in [SERVER_ID_START + 40_000, SERVER_ID_START + 0xffff] {
        let mut rig = rig();
        let mut events = new_offer(id, "text/plain");
        events.extend(selection(id));
        rig.peer().send(&events);

        assert_eq!(kinds(&drain_events(&mut rig.session)), [("selection", Some(id))]);
        assert_eq!(rig.session.get_offer().unwrap().offered_mime_types(), ["text/plain"]);
    }
}

#[test]
fn an_offer_id_past_the_cap_is_an_error() {
    for id in [SERVER_ID_START + 0x1_0000, u32::MAX] {
        let mut rig = rig();
        rig.peer().send(&device_event(DEVICE_DATA_OFFER, id));
        rig.peer().shutdown_write();
        let err = err_of(rig.session.next_event());
        assert_ne!(err.kind(), ErrorKind::UnexpectedEof, "{id:#x}: {err}");
    }
}

#[test]
fn a_new_offer_reusing_the_id_of_a_live_offer_is_an_error() {
    // Only we destroy offers, so a live id announced again means the compositor is broken.
    let mut rig = rig();
    let mut events = new_offer(OFFER_A, "text/plain");
    events.extend(selection(OFFER_A));
    events.extend(device_event(DEVICE_DATA_OFFER, OFFER_A));
    rig.peer().send(&events);
    rig.peer().shutdown_write();

    assert_eq!(kinds(&[rig.session.next_event().unwrap()]), [("selection", Some(OFFER_A))]);
    let err = err_of(rig.session.next_event());
    assert_ne!(err.kind(), ErrorKind::UnexpectedEof, "{err}");
}

#[test]
fn events_for_the_reserved_id_or_an_unannounced_server_id_are_errors() {
    // OFFER_C grows the server table past OFFER_B, which stays unannounced.
    for (case, event) in [
        // Shaped like an offer event, so only the id can make it an error.
        ("reserved id 0", mime_offer(0, "text/plain")),
        ("unannounced server id", mime_offer(OFFER_B, "text/plain")),
    ] {
        let mut rig = rig();
        let mut events = new_offer(OFFER_C, "text/plain");
        events.extend(event);
        rig.peer().send(&events);
        rig.peer().shutdown_write();
        let err = err_of(rig.session.next_event());
        assert_ne!(err.kind(), ErrorKind::UnexpectedEof, "{case}: {err}");
    }
}
