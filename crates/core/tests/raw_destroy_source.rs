//! `destroy_source`: the request on the wire, events that are still in flight for a destroyed
//! source (their fds must be consumed in order and closed), id reuse after `delete_id`, and ids
//! outside the router's tables. No relay here, so non-blocking EOF checks are valid.

mod helpers;

use std::{
    fs::File,
    io::Write,
    os::fd::{AsFd, BorrowedFd},
};

use clipleaf_core::WlSessionEvent;
use clipleaf_test_common::{
    fds,
    raw::{all_bytes, wire},
};
use helpers::*;

/// ext_data_control_source_v1.destroy, not among the shared protocol constants.
const SOURCE_DESTROY: u16 = 1;

#[test]
fn destroy_source_sends_a_destroy_request_of_8_bytes_for_that_source() {
    let mut rig = rig();
    let a = rig.session.create_data_source().unwrap();
    let b = rig.session.create_data_source().unwrap();
    let (a_id, b_id) = (a.local_id, b.local_id);
    rig.flush_and_drain();

    rig.session.destroy_source(a).unwrap();
    let batches = rig.flush_batches();
    assert_eq!(all_bytes(&batches), wire::msg(a_id, SOURCE_DESTROY, &[]));
    assert!(batches.iter().all(|batch| batch.fds.is_empty()));
    assert_ne!(a_id, b_id);
}

#[test]
fn a_destroyed_sources_in_flight_send_gives_up_its_fd_and_does_not_steal_the_next_ones() {
    let mut rig = rig();
    let a = rig.session.create_data_source().unwrap();
    let b = rig.session.create_data_source().unwrap();
    let (a_id, b_id) = (a.local_id, b.local_id);
    rig.flush_and_drain();
    rig.session.destroy_source(a).unwrap();
    rig.flush_and_drain();

    // The compositor had already sent these before it saw the destroy; one write, two fds in order.
    let (mut a_reader, a_fd) = fds::channel();
    let (mut b_reader, b_fd) = fds::channel();
    let mut events = source_send(a_id, b"text/plain");
    events.extend(source_cancelled(a_id));
    events.extend(source_send(b_id, b"text/plain"));
    let attached: [BorrowedFd; 2] = [a_fd.as_fd(), b_fd.as_fd()];
    rig.peer().send_with_fds(&events, &attached);
    drop((a_fd, b_fd));

    let WlSessionEvent::DataControlSend { source, mime, fd } = rig.session.next_event().unwrap() else {
        panic!("expected the send for the live source");
    };
    assert_eq!((source, &*mime), (b_id, &b"text/plain"[..]));
    File::from(fd).write_all(b"for B").unwrap();
    assert_eq!(fds::read_to_eof_now(&mut b_reader), b"for B");
    // A's fd was taken and closed by the session: nobody holds its write end any more.
    assert_eq!(fds::read_to_eof_now(&mut a_reader), b"");
    assert!(rig.session.try_next_event().unwrap().is_none());
}

#[test]
fn a_destroyed_sources_id_is_reused_only_after_delete_id() {
    let mut rig = rig();
    let a = rig.session.create_data_source().unwrap();
    let a_id = a.local_id;
    rig.session.destroy_source(a).unwrap();

    // Until the compositor confirms the delete, the id may still be in use on its side.
    let b = rig.session.create_data_source().unwrap();
    assert_ne!(b.local_id, a_id);
    rig.flush_and_drain();
    rig.peer().send(&wire::msg(DISPLAY, DISPLAY_DELETE_ID, &a_id.to_ne_bytes()));
    assert!(rig.session.try_next_event().unwrap().is_none());
    assert_eq!(rig.session.create_data_source().unwrap().local_id, a_id);
}

#[test]
fn an_event_for_an_id_beyond_the_routers_tables_is_an_error() {
    // 16 is the first client id and 0xff000010 the first server id past the 16-entry tables.
    for id in [16, SERVER_ID_START + 16] {
        let mut rig = rig();
        rig.peer().send(&wire::msg(id, 0, &[]));
        // If the event were skipped, the next read is EOF (another error) instead of a wait.
        rig.peer().shutdown_write();
        let err = err_of(rig.session.next_event());
        assert!(err.to_string().contains("out of bounds"), "{id}: {err}");
    }
}
