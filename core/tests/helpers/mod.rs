//! Shared by the raw-peer test files of this crate. (Test-only; lives under tests/, not in src.)
#![allow(dead_code)]

use std::io;

use clipleaf_core::{WlBufferedStream, WlSessionEvent, WlSessionManager};
pub use clipleaf_test_common::raw::proto::*;
use clipleaf_test_common::raw::{
    self, Batch, RawCompositor, RawPeer,
    wire::{self, Request},
};

/// A seat and the manager, both advertised at version 1.
pub const GLOBALS: [(u32, &str, u32); 2] =
    [(1, "wl_seat", 1), (54, "ext_data_control_manager_v1", 1)];

pub const OFFER_A: u32 = SERVER_ID_START;
pub const OFFER_B: u32 = SERVER_ID_START + 1;
pub const OFFER_C: u32 = SERVER_ID_START + 2;

pub struct Rig {
    pub session: WlSessionManager,
    peer: Option<RawPeer>,
    _comp: RawCompositor,
}

/// Connects, lets the peer answer the registry round trip, and initializes. Returns the
/// `initialize` error if there is one.
pub fn try_rig(globals: &[(u32, &str, u32)]) -> io::Result<Rig> {
    let comp = RawCompositor::new();
    let stream = WlBufferedStream::connect(comp.path())?;
    let peer = comp.accept();
    peer.send(&raw::registry_reply(globals));
    let session = WlSessionManager::initialize(stream)?;
    Ok(Rig {
        session,
        peer: Some(peer),
        _comp: comp,
    })
}

/// A session with the device created (ids: manager 4, seat 5, device 6), the handshake requests
/// already flushed and drained, so the write buffer and the peer's inbox are both empty.
pub fn rig() -> Rig {
    let mut rig = try_rig(&GLOBALS).expect("initialize");
    rig.flush_and_drain();
    rig
}

impl Rig {
    pub fn peer(&self) -> &RawPeer {
        self.peer.as_ref().expect("the peer hung up")
    }

    /// The compositor closes its end of the connection.
    pub fn hang_up(&mut self) {
        self.peer.take().expect("the peer hung up").close();
    }

    /// Sends what is queued and returns the requests the peer got.
    pub fn flush_and_drain(&mut self) -> Vec<Request> {
        self.session.send_messages().expect("send_messages");
        self.peer().requests()
    }

    /// Sends what is queued and returns the raw batches (with fds).
    pub fn flush_batches(&mut self) -> Vec<Batch> {
        self.session.send_messages().expect("send_messages");
        self.peer().recv_batches()
    }

    pub fn send_events(&self, events: &[Vec<u8>]) {
        self.peer().send(&events.concat());
    }
}

pub fn device_event(op: u16, arg: u32) -> Vec<u8> {
    wire::msg(DEVICE, op, &arg.to_ne_bytes())
}

pub fn mime_offer(offer: u32, mime: &str) -> Vec<u8> {
    wire::msg(offer, OFFER_OFFER, &wire::string(mime.as_bytes()))
}

/// `data_offer` plus one `offer` event, the way a compositor introduces an offer.
pub fn new_offer(id: u32, mime: &str) -> Vec<u8> {
    let mut m = device_event(DEVICE_DATA_OFFER, id);
    m.extend(mime_offer(id, mime));
    m
}

pub fn selection(id: u32) -> Vec<u8> {
    device_event(DEVICE_SELECTION, id)
}

pub fn primary_selection(id: u32) -> Vec<u8> {
    device_event(DEVICE_PRIMARY_SELECTION, id)
}

pub fn source_send(source: u32, mime: &[u8]) -> Vec<u8> {
    wire::msg(source, SOURCE_SEND, &wire::string(mime))
}

pub fn source_cancelled(source: u32) -> Vec<u8> {
    wire::msg(source, SOURCE_CANCELLED, &[])
}

pub fn done() -> Vec<u8> {
    raw::callback_done(CALLBACK)
}

/// Every event the session has ready right now, via `try_next_event`.
pub fn drain_events(session: &mut WlSessionManager) -> Vec<WlSessionEvent> {
    let mut events = Vec::new();
    while let Some(event) = session.try_next_event().expect("try_next_event") {
        events.push(event);
    }
    events
}

/// A short description of each event, for comparing sequences.
pub fn kinds(events: &[WlSessionEvent]) -> Vec<(&'static str, Option<u32>)> {
    events
        .iter()
        .map(|e| match e {
            WlSessionEvent::Selection { offer_id } => ("selection", *offer_id),
            WlSessionEvent::PrimarySelection { offer_id } => ("primary", *offer_id),
            WlSessionEvent::DataControlSend { source, .. } => ("send", Some(*source)),
            WlSessionEvent::DataControlCancelled { source } => ("cancelled", Some(*source)),
            WlSessionEvent::SyncDone => ("sync", None),
        })
        .collect()
}

/// Ids of the offers the client destroyed, in order.
pub fn destroyed_offers(requests: &[Request]) -> Vec<u32> {
    requests
        .iter()
        .filter(|r| r.object_id >= SERVER_ID_START && r.opcode == OFFER_DESTROY)
        .map(|r| r.object_id)
        .collect()
}

/// `Result::unwrap_err` for results whose Ok side is not `Debug`.
pub fn err_of<T>(result: io::Result<T>) -> io::Error {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}
