//! How `initialize` reads the registry: what it binds when the compositor advertises unknown,
//! duplicate or removed globals, and how a malformed registry event ends the session. (Unknown
//! globals next to the known ones are also covered by the `wl_output` in
//! `initialize_binds_the_manager_and_the_seat_and_creates_a_device`.)

mod helpers;

use std::io::ErrorKind;

use clipleaf_core::{WlBufferedStream, WlSessionManager};
use clipleaf_test_common::raw::{RawCompositor, callback_done, wire};
use helpers::*;

/// wl_registry.global_remove, not among the shared protocol constants.
const REGISTRY_GLOBAL_REMOVE: u16 = 1;

fn global(name: u32, interface: &str, version: u32) -> Vec<u8> {
    let mut body = name.to_ne_bytes().to_vec();
    body.extend(wire::string(interface.as_bytes()));
    body.extend(version.to_ne_bytes());
    wire::msg(REGISTRY, REGISTRY_GLOBAL, &body)
}

/// Connects, lets the peer play `events` and then answer the sync, and initializes. On success
/// returns the global names the client bound, in order: (manager, seat).
fn initialize_with(events: &[Vec<u8>]) -> std::io::Result<(u32, u32)> {
    let comp = RawCompositor::new();
    let stream = WlBufferedStream::connect(comp.path())?;
    let peer = comp.accept();
    let mut bytes = events.concat();
    bytes.extend(callback_done(CALLBACK));
    peer.send(&bytes);
    // Nothing else is coming: an initialize that misses its `done` fails on EOF instead of waiting.
    peer.shutdown_write();
    let mut session = WlSessionManager::initialize(stream)?;
    session.send_messages()?;
    let binds: Vec<u32> = peer
        .requests()
        .iter()
        .filter(|r| (r.object_id, r.opcode) == (REGISTRY, REGISTRY_BIND))
        .map(|r| r.u32_at(0))
        .collect();
    assert_eq!(binds.len(), 2, "{binds:?}");
    Ok((binds[0], binds[1]))
}

#[test]
fn unknown_globals_are_skipped_and_the_known_ones_are_still_bound() {
    let bound = initialize_with(&[
        global(3, "zxdg_unknown_v1", 7),
        global(1, "wl_seat", 1),
        global(4, "", 1),
        global(54, "ext_data_control_manager_v1", 1),
        global(5, "wl_seat_but_longer", 1),
    ]);
    assert_eq!(bound.unwrap(), (54, 1));
}

#[test]
fn when_a_global_is_advertised_twice_the_last_one_is_bound() {
    let bound = initialize_with(&[
        global(1, "wl_seat", 1),
        global(54, "ext_data_control_manager_v1", 1),
        global(2, "wl_seat", 1),
        global(55, "ext_data_control_manager_v1", 1),
    ]);
    assert_eq!(bound.unwrap(), (55, 2));
}

#[test]
fn global_remove_is_ignored() {
    // Protocol-legal. The client does not track removals, so the seat is still bound.
    let remove = wire::msg(REGISTRY, REGISTRY_GLOBAL_REMOVE, &1u32.to_ne_bytes());
    let bound = initialize_with(&[
        global(1, "wl_seat", 1),
        global(54, "ext_data_control_manager_v1", 1),
        remove,
    ]);
    assert_eq!(bound.unwrap(), (54, 1));
}

#[test]
fn a_malformed_global_ends_initialize_with_invalid_data() {
    // A name and an interface, but the message ends before the version. The valid globals come
    // first, so a parser that let it through would initialize successfully.
    let mut body = 2u32.to_ne_bytes().to_vec();
    body.extend(wire::string(b"wl_seat"));
    let truncated = wire::msg(REGISTRY, REGISTRY_GLOBAL, &body);
    let err = err_of(initialize_with(&[
        global(1, "wl_seat", 1),
        global(54, "ext_data_control_manager_v1", 1),
        truncated,
    ]));
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
}

#[test]
fn an_unknown_registry_opcode_ends_initialize_with_an_error() {
    let err = err_of(initialize_with(&[
        global(1, "wl_seat", 1),
        global(54, "ext_data_control_manager_v1", 1),
        wire::msg(REGISTRY, 9, &[]),
    ]));
    assert_eq!(err.kind(), ErrorKind::InvalidData, "{err}");
}
