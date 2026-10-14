//! `sync()` against the scripted raw peer: every call gets its own callback and each `SyncDone`
//! reports the callback it belongs to.

mod helpers;

use helpers::*;

#[test]
fn two_syncs_in_flight_each_report_their_own_callback_in_order() {
    let mut rig = rig();
    let first = rig.session.sync().unwrap();
    let second = rig.session.sync().unwrap();
    assert_ne!(first, second);

    // The ids on the wire are the ones `sync` returned.
    let requests = rig.peer().requests();
    let wire_ids: Vec<u32> = requests
        .iter()
        .inspect(|r| assert_eq!((r.object_id, r.opcode), (DISPLAY, DISPLAY_SYNC)))
        .map(|r| r.u32_at(0))
        .collect();
    assert_eq!(wire_ids, [first, second]);

    // The compositor answers each callback, with both answers in one write.
    rig.send_events(&[done(wire_ids[0]), done(wire_ids[1])]);

    // Each `done` names its own callback, so a waiter for the second sync can tell the first one's
    // `done` from its own. Exactly two reads: a wrong id fails here instead of waiting forever.
    let seen = [
        rig.session.next_event().unwrap(),
        rig.session.next_event().unwrap(),
    ];
    assert_eq!(
        kinds(&seen),
        [("sync", Some(first)), ("sync", Some(second))]
    );
    assert!(rig.session.try_next_event().unwrap().is_none());
}
