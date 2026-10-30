//! Client against a scripted fake daemon on a Unix socket.

mod common;

use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

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
    match c.next_event() {
        Ok(Event::Added(a)) => assert_eq!(a.entry, sample_entry(1)),
        _ => panic!("expected Added"),
    }
    assert_eq!(expect_removed(c.next_event()), 2);
    assert!(matches!(c.next_event(), Ok(Event::Cleared)));
    match c.next_event() {
        Ok(Event::SelectionChanged(s)) => assert_eq!((s.selection, s.id), (EventSelection::Primary, Some(4))),
        _ => panic!("expected SelectionChanged"),
    }
    match c.next_event() {
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
    assert_eq!(expect_removed(c.next_event()), 7);
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
    assert_eq!(expect_removed(c.next_event()), 1);
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
    assert_eq!(expect_removed(c.next_event()), 11);
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
    assert_eq!(expect_removed(c.next_event()), 5);
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
        assert_eq!(expect_removed(c.next_event()), i);
    }
    c.call(&Status {}).unwrap();
    assert_eq!(expect_removed(c.next_event()), 7777);
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
    assert!(matches!(c.next_event(), Err(ClientError::EventQueueFull)));
    assert_eq!(c.call(&Status {}).unwrap().entries, 2);
    // none of the 1100 dropped events resurface, and the gap is reported only once
    assert_eq!(expect_removed(c.next_event()), 5000);
    assert_eq!(expect_removed(c.next_event()), 5001);
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
    assert!(matches!(c.next_event(), Err(ClientError::EventQueueFull)));
    // sent after the overflow was reported -> delivered
    assert_eq!(expect_removed(c.next_event()), 9000);
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
    let ev = c.next_event();
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
    assert_eq!(expect_removed(c.next_event()), 1);
    assert!(matches!(c.next_event(), Err(ClientError::EofClosed)));
    assert!(matches!(c.next_event(), Err(ClientError::Poisoned)));
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
    c.set_timeout(Some(GUARD)).unwrap();
    assert_eq!(c.call(&Status {}).unwrap().entries, 222);
    d.join();
}

#[test]
fn timeout_in_next_event_does_not_poison() {
    let (tx, rx) = mpsc::channel::<()>();
    let d = Daemon::spawn(move |mut p| {
        p.handshake();
        rx.recv().unwrap();
        p.send(0, &Removed { id: 3 });
        p.wait_close();
    });
    let mut c = Client::connect_handshake(&d.path, 4096, 4096, Some(Duration::from_millis(100))).unwrap();
    assert!(matches!(c.next_event(), Err(ClientError::Timeout)));
    tx.send(()).unwrap();
    c.set_timeout(Some(GUARD)).unwrap();
    assert_eq!(expect_removed(c.next_event()), 3);
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
        for _ in 0..8 {
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
    let _ = c.call(&Get { id: 1, mime: None });
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
        RequestType::Get,
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
