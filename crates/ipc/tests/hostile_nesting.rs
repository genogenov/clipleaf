//! A deeply nested body is a decode error, never a stack overflow. Own test binary: a
//! regression aborts the process and would hide other tests.
//!
//! Runs on a 2 MiB thread, the default for spawned threads (the daemon's connection threads,
//! a GUI's worker) and test threads. Before `decode_body` capped the depth at 32, rmp-serde's
//! own limit (1024) let a < 1 KB body overflow that stack in a debug build at a depth of
//! 700-900 (measured 2026-10-29), aborting the process.

use clipleaf_ipc::protocol::{codec::decode_body, v1::models::Delete};

fn nested_unknown_field(depth: usize, map: bool) -> Vec<u8> {
    // {"x": [[[...nil...]]], "id": 1} with the nesting as an unknown field value.
    let mut body = vec![0x82, 0xa1, b'x'];
    for _ in 0..depth {
        if map {
            body.extend([0x81, 0xa1, b'k']);
        } else {
            body.push(0x91);
        }
    }
    body.push(0xc0);
    body.extend([0xa2, b'i', b'd', 1]);
    body
}

#[test]
fn deeply_nested_body_is_an_error_not_a_stack_overflow() {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            for map in [false, true] {
                // shallow nesting in an unknown field is skipped
                assert_eq!(decode_body::<Delete>(&nested_unknown_field(16, map)).unwrap(), Delete { id: 1 });
                for depth in [64, 800, 1024, 20_000] {
                    assert!(decode_body::<Delete>(&nested_unknown_field(depth, map)).is_err(), "depth {depth}");
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
