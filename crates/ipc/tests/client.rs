//! Client against a scripted fake daemon on a Unix socket.

mod common;

use std::{
    os::fd::AsFd,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};
use clipleaf_ipc::{
    client::{Client, Event, error::ClientError},
    protocol::{
        error::ProtocolError,
        handshake::{ErrorCode, ErrorReply, Hello},
        header::{MessageType, RequestType},
        v1::models::*,
    },
};
use common::{Daemon, GUARD, frame, raw_frame, sample_entry};

fn status_reply(entries: u64) -> StatusReply {
    StatusReply {
        entries,
        pinned: 0,
        history_limit: 100,
        clipboard_id: None,
        primary_id: None,
    }
}

fn list() -> List {
    List {
        query: None,
        offset: 0,
        limit: None,
        thumbs: false,
    }
}

/// `next_event` bounded by GUARD, so a broken client fails the test instead of hanging it.
fn next(c: &mut Client) -> Result<Event, ClientError> {
    c.next_event(Some(GUARD)).map(|ev| ev.expect("no event within GUARD"))
}

fn expect_removed(ev: Result<Event, ClientError>) -> u64 {
    match ev {
        Ok(Event::Removed(r)) => r.id,
        Ok(_) => panic!("expected Removed, got another event"),
        Err(e) => panic!("expected Removed, got {e:?}"),
    }
}

// ---- Handshake ----

#[test]
fn handshake_ok_sends_hello_with_version_1() {
    let d = Daemon::spawn(|mut p| p.handshake());
    let client = d.connect();
    drop(client);
    let (req_id, hello) = d.join();
    assert_ne!(req_id, 0);
    assert_eq!(hello.versions, vec![1]);
}

#[test]
fn handshake_version_mismatch_is_unsupported_version() {
    let d = Daemon::spawn(|mut p| {
        p.handshake_with(2);
    });
    let res = Client::connect_handshake(&d.path, 4096, 4096, Some(GUARD));
    assert!(matches!(res, Err(ClientError::UnsupportedVersion)), "{:?}", res.err());
    d.join();
}

#[test]
fn connect_to_missing_socket_is_io_error() {
    let dir = common::TempDir::new();
    let res = Client::connect_handshake(&dir.path().join("nope.sock"), 4096, 4096, Some(GUARD));
    assert!(matches!(res, Err(ClientError::Io(_))), "{:?}", res.err());
}

// ---- Calls and events ----

#[test]
fn events_before_reply_are_queued_in_order() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        p.send(0, &Added { entry: sample_entry(1) });
        p.send(0, &Removed { id: 2 });
        p.send(0, &Cleared {});
        p.send(
            0,
            &SelectionChanged {
                selection: EventSelection::Primary,
                id: Some(4),
            },
        );
        p.send(0, &Updated { entry: sample_entry(5) });
        p.send(id, &status_reply(9));
    });
    let mut c = d.connect();
    assert_eq!(c.call(&Status {}).unwrap(), status_reply(9));
    match next(&mut c) {
        Ok(Event::Added(a)) => assert_eq!(a.entry, sample_entry(1)),
        _ => panic!("expected Added"),
    }
    assert_eq!(expect_removed(next(&mut c)), 2);
    assert!(matches!(next(&mut c), Ok(Event::Cleared)));
    match next(&mut c) {
        Ok(Event::SelectionChanged(s)) => assert_eq!((s.selection, s.id), (EventSelection::Primary, Some(4))),
        _ => panic!("expected SelectionChanged"),
    }
    match next(&mut c) {
        Ok(Event::Updated(u)) => assert_eq!(u.entry, sample_entry(5)),
        _ => panic!("expected Updated"),
    }
    d.join();
}

/// Regression: an event and the reply arriving in one read must not make call() block for
/// more data (it would hit the timeout).
#[test]
fn event_and_reply_in_one_write_returns_promptly() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut both = frame(0, &Removed { id: 7 });
        both.extend(frame(id, &status_reply(1)));
        p.send_raw(&both);
        // keep the socket open until the client is done, so EOF can't mask a hang
        p.wait_close();
    });
    let mut c = d.connect();
    let start = Instant::now();
    let res = c.call(&Status {});
    let took = start.elapsed();
    assert!(res.is_ok(), "{:?}", res.err());
    assert!(took < Duration::from_millis(500), "call took {took:?}");
    assert_eq!(expect_removed(next(&mut c)), 7);
    drop(c);
    d.join();
}

#[test]
fn handshake_reply_and_event_in_one_write() {
    let d = Daemon::spawn(|mut p| {
        let (id, _) = p.recv_msg::<Hello>();
        let mut both = frame(
            id,
            &clipleaf_ipc::protocol::handshake::HelloReply {
                version: 1,
                features: vec![],
                daemon: None,
            },
        );
        both.extend(frame(0, &Removed { id: 1 }));
        p.send_raw(&both);
        p.wait_close();
    });
    let mut c = d.connect();
    // the event read together with the HelloReply is not lost
    assert_eq!(expect_removed(next(&mut c)), 1);
    drop(c);
    d.join();
}

#[test]
fn next_event_blocks_until_an_event_arrives() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        thread::sleep(Duration::from_millis(50));
        p.send(0, &Removed { id: 11 });
        p.wait_close();
    });
    let mut c = d.connect();
    assert_eq!(expect_removed(next(&mut c)), 11);
    drop(c);
    d.join();
}

#[test]
fn stale_reply_with_other_req_id_is_dropped() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        p.send(id.wrapping_add(1000), &status_reply(666));
        p.send(id - 1, &status_reply(667));
        p.send(id, &status_reply(1));
    });
    let mut c = d.connect();
    assert_eq!(c.call(&Status {}).unwrap().entries, 1);
    d.join();
}

#[test]
fn error_with_our_req_id_is_response_error_and_connection_stays_usable() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<List>();
        p.send(
            id,
            &ErrorReply {
                code: ErrorCode::NotFound,
                message: "gone".into(),
            },
        );
        let (id, _) = p.recv_msg::<Status>();
        p.send(id, &status_reply(3));
    });
    let mut c = d.connect();
    match c.call(&list()) {
        Err(ClientError::ResponseError(e)) => assert_eq!(
            e,
            ErrorReply {
                code: ErrorCode::NotFound,
                message: "gone".into()
            }
        ),
        other => panic!("expected ResponseError, got {:?}", other.err()),
    }
    assert_eq!(c.call(&Status {}).unwrap().entries, 3);
    d.join();
}

#[test]
fn error_with_req_id_0_during_call_is_connection_error() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        p.recv_msg::<List>();
        p.send(
            0,
            &ErrorReply {
                code: ErrorCode::Internal,
                message: "bye".into(),
            },
        );
    });
    let mut c = d.connect();
    match c.call(&list()) {
        Err(ClientError::ConnectionError(e)) => assert_eq!(e.code, ErrorCode::Internal),
        other => panic!("expected ConnectionError, got {:?}", other.err()),
    }
    // the daemon closes after a connection error: the client is poisoned, not left to hit EPIPE
    assert!(matches!(c.call(&Status {}), Err(ClientError::Poisoned)));
    d.join();
}

#[test]
fn reply_of_wrong_type_with_our_req_id_is_unexpected_type() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<List>();
        p.send(id, &status_reply(1));
        let (id, _) = p.recv_msg::<List>();
        p.send(id, &list()); // a Request echoed back
        let (id, _) = p.recv_msg::<List>();
        p.send_raw(&raw_frame([5, 1], id, &[0x80])); // unknown category
        let (id, _) = p.recv_msg::<List>();
        p.send_raw(&raw_frame([2, 10], id, &[0x80])); // unknown reply index
    });
    let mut c = d.connect();
    let expected = [
        MessageType::Reply(RequestType::Status),
        MessageType::Request(RequestType::List),
        MessageType::Unknown(0x0501),
        MessageType::Unknown(0x020A),
    ];
    for want in expected {
        match c.call(&list()) {
            Err(ClientError::UnexpectedType(t)) => assert_eq!(t, want),
            other => panic!("expected UnexpectedType({want:?}), got {:?}", other.err()),
        }
    }
    d.join();
}

#[test]
fn unknown_event_index_and_unknown_category_with_req_id_0_are_skipped() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        p.send_raw(&raw_frame([4, 6], 0, &[0x81, 0xa1, b'x', 1]));
        p.send_raw(&raw_frame([4, 255], 0, &[]));
        p.send_raw(&raw_frame([5, 1], 0, &[0x80]));
        p.send_raw(&raw_frame([9, 9], 0, &[1, 2, 3]));
        p.send(0, &Removed { id: 5 });
        p.send(id, &status_reply(2));
    });
    let mut c = d.connect();
    assert_eq!(c.call(&Status {}).unwrap().entries, 2);
    // only the known event was queued
    assert_eq!(expect_removed(next(&mut c)), 5);
    d.join();
}

#[test]
fn undecodable_reply_body_is_protocol_decode_error() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        p.send_raw(&raw_frame([2, 2], id, &[0x80])); // StatusReply missing required fields
    });
    let mut c = d.connect();
    let res = c.call(&Status {});
    assert!(matches!(res, Err(ClientError::Protocol(ProtocolError::Decode(_)))), "{:?}", res.err());
    d.join();
}

// ---- Event queue overflow ----

#[test]
fn exactly_1024_queued_events_do_not_overflow() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut batch = Vec::new();
        for i in 0..1024 {
            batch.extend(frame(0, &Removed { id: i }));
        }
        batch.extend(frame(id, &status_reply(1)));
        p.send_raw(&batch);
        let (id, _) = p.recv_msg::<Status>();
        p.send(0, &Removed { id: 7777 });
        p.send(id, &status_reply(2));
    });
    let mut c = d.connect();
    c.call(&Status {}).unwrap();
    for i in 0..1024 {
        assert_eq!(expect_removed(next(&mut c)), i);
    }
    c.call(&Status {}).unwrap();
    assert_eq!(expect_removed(next(&mut c)), 7777);
    d.join();
}

#[test]
fn event_queue_overflow_reports_once_then_delivers_new_events() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut batch = Vec::new();
        for i in 0..1100 {
            batch.extend(frame(0, &Removed { id: i }));
        }
        batch.extend(frame(id, &status_reply(1)));
        p.send_raw(&batch);
        // the client has now observed EventQueueFull and asks again
        let (id, _) = p.recv_msg::<Status>();
        p.send(0, &Removed { id: 5000 });
        p.send(0, &Removed { id: 5001 });
        p.send(id, &status_reply(2));
    });
    let mut c = d.connect();
    assert_eq!(c.call(&Status {}).unwrap().entries, 1);
    assert!(matches!(next(&mut c), Err(ClientError::EventQueueFull)));
    assert_eq!(c.call(&Status {}).unwrap().entries, 2);
    // none of the 1100 dropped events resurface, and the gap is reported only once
    assert_eq!(expect_removed(next(&mut c)), 5000);
    assert_eq!(expect_removed(next(&mut c)), 5001);
    d.join();
}

#[test]
fn exactly_1025_events_overflow() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut batch = Vec::new();
        for i in 0..1025 {
            batch.extend(frame(0, &Removed { id: i }));
        }
        batch.extend(frame(id, &status_reply(1)));
        p.send_raw(&batch);
        p.send(0, &Removed { id: 9000 });
        p.wait_close();
    });
    let mut c = d.connect();
    c.call(&Status {}).unwrap();
    assert!(matches!(next(&mut c), Err(ClientError::EventQueueFull)));
    // sent after the overflow was reported -> delivered
    assert_eq!(expect_removed(next(&mut c)), 9000);
    drop(c);
    d.join();
}

// ---- Fatal conditions ----

#[test]
fn daemon_close_is_eof_then_poisoned() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        p.recv_msg::<Status>();
        // drop the stream: EOF for the client
    });
    let mut c = d.connect();
    let res = c.call(&Status {});
    assert!(matches!(res, Err(ClientError::EofClosed)), "{:?}", res.err());
    d.join();
    let res = c.call(&Status {});
    assert!(matches!(res, Err(ClientError::Poisoned)), "{:?}", res.err());
    let ev = next(&mut c);
    assert!(matches!(ev, Err(ClientError::Poisoned)), "{:?}", ev.err());
}

#[test]
fn eof_in_next_event_is_eof_closed() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        p.send(0, &Removed { id: 1 });
    });
    let mut c = d.connect();
    d.join();
    assert_eq!(expect_removed(next(&mut c)), 1);
    assert!(matches!(next(&mut c), Err(ClientError::EofClosed)));
    assert!(matches!(next(&mut c), Err(ClientError::Poisoned)));
}

#[test]
fn oversized_frame_header_is_frame_too_big_and_poisons() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut h = (32u32 * 1024 * 1024).to_le_bytes().to_vec(); // body > 32 MiB - 12
        h.extend([2, 2, 0, 0]);
        h.extend(id.to_le_bytes());
        p.send_raw(&h);
        let _ = p.stream.shutdown(std::net::Shutdown::Write);
        // drain until the client goes away
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut p.stream, &mut sink);
    });
    let mut c = d.connect();
    let start = Instant::now();
    let res = c.call(&Status {});
    assert!(matches!(res, Err(ClientError::Protocol(ProtocolError::FrameTooBig))), "{:?}", res.err());
    assert!(start.elapsed() < Duration::from_millis(500));
    let res = c.call(&Status {});
    assert!(matches!(res, Err(ClientError::Poisoned)), "{:?}", res.err());
    drop(c);
    d.join();
}

#[test]
fn timeout_does_not_poison_and_late_reply_is_dropped() {
    let (tx, rx) = mpsc::channel::<()>();
    let d = Daemon::spawn(move |mut p| {
        p.handshake();
        let (late, _) = p.recv_msg::<Status>();
        rx.recv().unwrap(); // until the client reported Timeout
        p.send(late, &status_reply(111));
        let (id, _) = p.recv_msg::<Status>();
        assert_ne!(id, late);
        p.send(id, &status_reply(222));
    });
    let mut c = Client::connect_handshake(&d.path, 4096, 4096, Some(Duration::from_millis(200))).unwrap();
    let start = Instant::now();
    let res = c.call(&Status {});
    let took = start.elapsed();
    assert!(matches!(res, Err(ClientError::Timeout)), "{:?}", res.err());
    assert!(
        took >= Duration::from_millis(150) && took < Duration::from_millis(1500),
        "timeout after {took:?}"
    );
    tx.send(()).unwrap();
    c.set_call_timeout(Some(GUARD)).unwrap();
    assert_eq!(c.call(&Status {}).unwrap().entries, 222);
    d.join();
}

#[test]
fn next_event_timeout_is_none_and_does_not_poison() {
    let (tx, rx) = mpsc::channel::<()>();
    let d = Daemon::spawn(move |mut p| {
        p.handshake();
        rx.recv().unwrap();
        p.send(0, &Removed { id: 3 });
        p.wait_close();
    });
    // The call timeout (SO_RCVTIMEO) must not bound next_event: only its own argument does.
    let mut c = Client::connect_handshake(&d.path, 4096, 4096, Some(Duration::from_millis(10))).unwrap();
    assert!(matches!(c.next_event(Some(Duration::ZERO)), Ok(None)));
    let start = Instant::now();
    assert!(matches!(c.next_event(Some(Duration::from_millis(200))), Ok(None)));
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(150) && took < Duration::from_millis(1500),
        "timeout after {took:?}"
    );
    tx.send(()).unwrap();
    assert_eq!(expect_removed(next(&mut c)), 3);
    drop(c);
    d.join();
}

// ---- Request ids ----

#[test]
fn req_ids_count_up_from_1_and_are_never_0() {
    let d = Daemon::spawn(|mut p| {
        let (hello_id, _) = p.handshake();
        let mut ids = vec![hello_id];
        for _ in 0..5 {
            let f = p.recv();
            ids.push(f.req_id);
            p.send(
                f.req_id,
                &ErrorReply {
                    code: ErrorCode::Internal,
                    message: String::new(),
                },
            );
        }
        ids
    });
    let mut c = d.connect();
    let _ = c.call(&Status {});
    let _ = c.call(&list());
    let _ = c.call(&Delete { id: 1 });
    let _ = c.call(&Pin { id: 1, pinned: true });
    let _ = c.call(&Subscribe { thumbs: false });
    let ids = d.join();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn every_request_type_reaches_the_daemon_with_its_type() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let mut types = Vec::new();
        for _ in 0..7 {
            let f = p.recv();
            types.push(f.msg_type);
            p.send(
                f.req_id,
                &ErrorReply {
                    code: ErrorCode::Cancelled,
                    message: String::new(),
                },
            );
        }
        types
    });
    let mut c = d.connect();
    let _ = c.call(&Status {});
    let _ = c.call(&list());
    let _ = c.call(&Restore {
        id: 1,
        selection: Selection::Both,
        formats: Formats::All,
        mime: None,
    });
    let _ = c.call(&Pin { id: 1, pinned: false });
    let _ = c.call(&Delete { id: 1 });
    let _ = c.call(&Clear { include_pinned: false });
    let _ = c.call(&Subscribe { thumbs: false });
    let want: Vec<_> = [
        RequestType::Status,
        RequestType::List,
        RequestType::Restore,
        RequestType::Pin,
        RequestType::Delete,
        RequestType::Clear,
        RequestType::Subscribe,
    ]
    .into_iter()
    .map(MessageType::Request)
    .collect();
    assert_eq!(d.join(), want);
}

#[test]
fn oversized_request_is_rejected_locally_and_connection_stays_usable() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        // the oversized List never reaches the daemon; the next frame is the Status
        let (id, _) = p.recv_msg::<Status>();
        p.send(id, &status_reply(4));
    });
    let mut c = d.connect();
    let big = List {
        query: Some("q".repeat(64 * 1024)),
        offset: 0,
        limit: None,
        thumbs: false,
    };
    let res = c.call(&big);
    assert!(matches!(res, Err(ClientError::Protocol(ProtocolError::FrameTooBig))), "{:?}", res.err());
    assert_eq!(c.call(&Status {}).unwrap().entries, 4);
    d.join();
}

// ---- Daemon name and AsFd (G1) ----

#[test]
fn daemon_name_is_the_hello_reply_daemon() {
    let d = Daemon::spawn(|mut p| {
        p.handshake(); // replies daemon: Some("fake 0.0.0")
        p.wait_close();
    });
    let c = d.connect();
    assert_eq!(c.daemon_name(), Some("fake 0.0.0"));
    drop(c);
    d.join();

    let d = Daemon::spawn(|mut p| {
        let (id, _) = p.recv_msg::<Hello>();
        p.send(
            id,
            &clipleaf_ipc::protocol::handshake::HelloReply {
                version: 1,
                features: vec![],
                daemon: None,
            },
        );
        p.wait_close();
    });
    let c = d.connect();
    assert_eq!(c.daemon_name(), None);
    drop(c);
    d.join();
}

/// One non-blocking poll of the client's fd for POLLIN.
fn fd_readable(c: &Client) -> bool {
    let mut fds = [PollFd::new(c.as_fd(), PollEvents::IN)];
    match poll(&mut fds, Some(Duration::ZERO)).expect("poll") {
        PollResult::Timeout => false,
        PollResult::Ready(_) => {
            let revents = fds[0].revents();
            assert!(revents.has_any(PollEvents::IN), "fd ready without POLLIN");
            true
        }
    }
}

#[test]
fn as_fd_is_readable_when_an_event_arrives_and_not_after_draining() {
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (sent_tx, sent_rx) = mpsc::channel::<()>();
    let d = Daemon::spawn(move |mut p| {
        p.handshake();
        // Only after the client finished the handshake, so the event can't ride in its read.
        go_rx.recv().unwrap();
        p.send(0, &Removed { id: 7 });
        sent_tx.send(()).unwrap();
        p.wait_close();
    });
    let mut c = d.connect();
    assert!(!fd_readable(&c), "readable before the daemon sent anything");
    go_tx.send(()).unwrap();
    sent_rx.recv().unwrap(); // the event is in the socket buffer now (unix delivery is synchronous)

    assert!(fd_readable(&c), "an event in the socket must make the fd readable");
    assert_eq!(expect_removed(c.next_event(Some(Duration::ZERO)).map(|e| e.expect("event"))), 7);
    assert!(matches!(c.next_event(Some(Duration::ZERO)), Ok(None)));
    assert!(!fd_readable(&c), "still readable after the event was read");
    drop(c);
    d.join();
}

/// Pins the AsFd doc: events that `call()` read ahead sit in the client's queue, so the fd is
/// NOT readable afterwards, yet `next_event(Some(ZERO))` returns them.
#[test]
fn event_read_ahead_by_call_is_queued_without_fd_readiness() {
    let d = Daemon::spawn(|mut p| {
        p.handshake();
        let (id, _) = p.recv_msg::<Status>();
        let mut both = frame(0, &Removed { id: 9 });
        both.extend(frame(id, &status_reply(3)));
        p.send_raw(&both); // one write: the client reads both in one go
        p.wait_close();
    });
    let mut c = d.connect();
    assert_eq!(c.call(&Status {}).unwrap(), status_reply(3));
    assert!(!fd_readable(&c), "the read-ahead event must not leave bytes in the socket");
    assert_eq!(expect_removed(c.next_event(Some(Duration::ZERO)).map(|e| e.expect("queued event"))), 9);
    assert!(matches!(c.next_event(Some(Duration::ZERO)), Ok(None)));
    drop(c);
    d.join();
}
