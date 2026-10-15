//! The session API as owner and as paster through the wayland-server relay
//! (`clipleaf_test_common::compositor()`): the user-scenario flows, with an independent
//! production parser reading clipleaf's requests. The relay runs on its own thread, so every
//! EOF check here is a BLOCKING read (the relay dups forwarded fds: EOF can be microseconds late).

use std::{
    io::{ErrorKind, Write},
    os::fd::OwnedFd,
};

use clipleaf_core::{OFFERED_TXT_MIME_TYPES, WlBufferedStream, WlSessionEvent, WlSessionManager};
use clipleaf_test_common::{Config, Note, TestCompositor, compositor, compositor_with, fds};

fn session(comp: &TestCompositor) -> WlSessionManager {
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    WlSessionManager::initialize(stream).unwrap()
}

fn until_sync(s: &mut WlSessionManager) {
    let callback_id = s.schedule_sync().unwrap();
    while !matches!(s.next_event().unwrap(), WlSessionEvent::SyncDone { callback_id: id } if id == callback_id)
    {
    }
}

/// An owner that offers `mimes` as the clipboard (or primary) and has synced.
fn owner_of(comp: &TestCompositor, mimes: &[&str], primary: bool) -> (WlSessionManager, u32) {
    let mut owner = session(comp);
    let source = owner.create_data_source().unwrap();
    for mime in mimes {
        source.offer(owner.get_message_writer(), mime).unwrap();
    }
    if primary {
        owner.set_primary_selection(source.local_id).unwrap();
    } else {
        owner.set_selection(source.local_id).unwrap();
    }
    until_sync(&mut owner);
    (owner, source.local_id)
}

/// Waits for the next `send` the owner is asked to serve; returns its mime and fd.
fn next_send(owner: &mut WlSessionManager) -> (Vec<u8>, OwnedFd) {
    loop {
        if let WlSessionEvent::DataControlSend { mime, fd, .. } = owner.next_event().unwrap() {
            return (mime.into_vec(), fd);
        }
    }
}

#[test]
fn an_owner_serves_a_paster_through_the_relay() {
    let comp = compositor();
    let (mut owner, _) = owner_of(&comp, &["text/plain"], false);

    let mut paster = session(&comp);
    until_sync(&mut paster);
    assert_eq!(
        paster.get_offer().unwrap().offered_mime_types(),
        ["text/plain"]
    );
    let (mut reader, writer) = fds::channel();
    let asked = paster.receive_offer(false, &None, writer).unwrap();
    assert_eq!(asked.map(|m| m.as_ref()), Some("text/plain"));
    paster.flush().unwrap();

    let (mime, fd) = next_send(&mut owner);
    assert_eq!(mime, b"text/plain");
    std::fs::File::from(fd).write_all(b"hi").unwrap();
    assert_eq!(fds::read_to_eof(&mut reader), b"hi");
    comp.wait_for(|n| matches!(n, Note::Forwarded { mime } if mime == "text/plain"));
}

/// Syncs and returns the events up to and including the sync's `done`. The relay answers a
/// client's requests in order, so anything it queued for this client earlier is among them.
fn events_until_sync(s: &mut WlSessionManager) -> Vec<WlSessionEvent> {
    let callback_id = s.schedule_sync().unwrap();
    let mut events = Vec::new();
    loop {
        let event = s.next_event().unwrap();
        let done =
            matches!(event, WlSessionEvent::SyncDone { callback_id: id } if id == callback_id);
        events.push(event);
        if done {
            return events;
        }
    }
}

#[test]
fn replacing_the_selection_cancels_the_previous_owner_and_only_that_one() {
    let comp = compositor();
    let (mut owner, source) = owner_of(&comp, &["text/plain"], false);

    let (mut second, _) = owner_of(&comp, &["text/plain"], false);
    // The cancel was queued for the first owner before the second one's sync was answered.
    let events = events_until_sync(&mut owner);
    assert!(
        events.iter().any(
            |e| matches!(e, WlSessionEvent::DataControlCancelled { source: id } if *id == source)
        ),
        "the first owner was not cancelled"
    );
    comp.wait_for(|n| matches!(n, Note::Cancelled { primary: false }));
    // The new owner is still current: nothing cancelled it.
    let events = events_until_sync(&mut second);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, WlSessionEvent::DataControlCancelled { .. })),
        "the new owner was cancelled"
    );
}

#[test]
fn the_clipboard_and_the_primary_selection_are_independent() {
    let comp = compositor();
    let (_clip, _) = owner_of(&comp, &["text/plain"], false);
    let (_prim, _) = owner_of(&comp, &["text/html"], true);

    let mut paster = session(&comp);
    until_sync(&mut paster);
    assert_eq!(
        paster.get_offer().unwrap().offered_mime_types(),
        ["text/plain"]
    );
    assert_eq!(
        paster.get_primary_offer().unwrap().offered_mime_types(),
        ["text/html"]
    );
}

#[test]
fn an_owner_that_disconnects_clears_the_selection() {
    let comp = compositor();
    let (owner, _) = owner_of(&comp, &["text/plain"], false);
    let mut paster = session(&comp);
    until_sync(&mut paster);
    assert!(paster.get_offer().is_some());

    drop(owner);
    comp.wait_cleared(false);
    let events = events_until_sync(&mut paster);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, WlSessionEvent::Selection { offer_id: None })),
        "the paster was not told the selection is gone"
    );
    assert!(paster.get_offer().is_none());
    let (_reader, writer) = fds::channel();
    let err = paster
        .receive_offer(false, &None, writer)
        .expect_err("must fail");
    assert!(err.to_string().contains("nothing found to paste"), "{err}");
}

#[test]
fn a_new_selection_reaches_a_paster_that_is_already_listening() {
    let comp = compositor();
    let mut paster = session(&comp);
    until_sync(&mut paster);
    assert!(paster.get_offer().is_none());

    let (_owner, _) = owner_of(&comp, &OFFERED_TXT_MIME_TYPES, false);
    loop {
        if let WlSessionEvent::Selection { offer_id: Some(_) } = paster.next_event().unwrap() {
            break;
        }
    }
    assert_eq!(
        paster.get_offer().unwrap().offered_mime_types(),
        OFFERED_TXT_MIME_TYPES
    );
}

#[test]
fn selection_churn_destroys_every_replaced_offer_at_the_compositor() {
    let comp = compositor();
    let mut paster = session(&comp);
    until_sync(&mut paster);

    // More rounds than the router (16) and the offer tracker (8) have slots. Each round a new
    // owner takes over while the previous one is still connected (so the selection is replaced,
    // not cleared).
    let rounds = 40;
    let mut previous = None;
    for round in 0..rounds {
        let (owner, _) = owner_of(&comp, &["text/plain"], false);
        loop {
            if let WlSessionEvent::Selection { offer_id: Some(_) } = paster.next_event().unwrap() {
                break;
            }
        }
        assert!(paster.get_offer().is_some(), "round {round}");
        previous = Some(owner);
    }
    // Each selection after the first replaced one: the paster destroyed it and said so. Its
    // last destroy is flushed when it goes back to waiting.
    paster.flush().unwrap();
    until_sync(&mut paster);
    // The relay processed every destroy before it answered the sync, so all are queued by now.
    let destroyed = comp
        .notes
        .try_iter()
        .filter(|n| matches!(n, Note::OfferDestroyed))
        .count();
    // Per round after the first: the paster destroys the offer it replaces, and so does the new
    // owner, whose own selection replaces the previous owner's offer it was handed at bind.
    assert_eq!(destroyed, 2 * (rounds - 1));
    drop(previous);
}

#[test]
fn raw_requests_pass_the_relays_parser_and_a_big_offer_list_survives_overflow() {
    // 100 types of ~40 bytes overflow the 4096-byte write buffer; the relay (a different
    // implementation) must see every one of them, in order.
    let comp = compositor();
    let mimes: Vec<String> = (0..100)
        .map(|i| format!("application/x-clipleaf-test-{i:03}"))
        .collect();
    let refs: Vec<&str> = mimes.iter().map(String::as_str).collect();
    let (_owner, _) = owner_of(&comp, &refs, false);
    assert_eq!(comp.wait_selection(false), mimes);
}

#[test]
fn initialize_without_ext_data_control_names_the_missing_protocol() {
    let comp = compositor_with(Config {
        data_control: false,
        ..Config::default()
    });
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    let Err(err) = WlSessionManager::initialize(stream) else {
        panic!("initialized without ext_data_control_manager_v1");
    };
    assert!(
        err.to_string().contains("ext_data_control_manager_v1"),
        "{err}"
    );
}

#[test]
fn initialize_without_a_seat_is_not_found() {
    let comp = compositor_with(Config {
        seat: false,
        ..Config::default()
    });
    let stream = WlBufferedStream::connect(&comp.socket_path()).unwrap();
    let Err(err) = WlSessionManager::initialize(stream) else {
        panic!("initialized without a seat");
    };
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn a_receive_on_an_offer_the_device_no_longer_holds_is_not_forwarded() {
    // Like wlroots: once a newer selection replaced the offer, `receive` on the old offer only
    // closes the fd. The paster has not read the news yet, so it still asks the old offer.
    let comp = compositor();
    let (mut first_owner, _) = owner_of(&comp, &["text/plain"], false);
    let mut paster = session(&comp);
    until_sync(&mut paster);
    let (_second_owner, _) = owner_of(&comp, &["text/plain"], false);

    let (mut reader, writer) = fds::channel();
    paster.receive_offer(false, &None, writer).unwrap();
    // The relay handled the receive before it answers this sync. Check the notes before reading
    // the payload, so a regression fails here instead of waiting for a `send` nobody serves.
    // (The paster ignores the news that came meanwhile; only the old offer matters here.)
    let callback_id = paster.schedule_sync().unwrap();
    comp.wait_for(|n| matches!(n, Note::Selection { mimes: Some(_), .. }));
    while !matches!(paster.next_event().unwrap(), WlSessionEvent::SyncDone { callback_id: id } if id == callback_id)
    {
    }
    assert!(
        !comp
            .notes
            .try_iter()
            .any(|n| matches!(n, Note::Forwarded { .. })),
        "a receive on a replaced offer was forwarded"
    );
    assert_eq!(fds::read_to_eof(&mut reader), b"");
    let events = events_until_sync(&mut first_owner);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, WlSessionEvent::DataControlSend { .. })),
        "the old owner was asked to serve"
    );
}
