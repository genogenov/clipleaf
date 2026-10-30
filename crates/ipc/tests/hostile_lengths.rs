//! Bodies that declare huge collection/byte lengths with no data behind them must fail fast
//! without a huge allocation. Its own test binary so the peak-RSS check sees only this test.

use std::time::{Duration, Instant};

use clipleaf_ipc::protocol::{
    codec::decode_body,
    handshake::Hello,
    v1::models::{GetReply, List, ListReply},
};

fn vm_hwm_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn key(k: &str) -> Vec<u8> {
    assert!(k.len() < 32);
    let mut v = vec![0xa0 | k.len() as u8];
    v.extend(k.as_bytes());
    v
}

/// A one-field map {k: <prefix>} where prefix is a length header with no data.
fn one_field(k: &str, prefix: &[u8]) -> Vec<u8> {
    let mut v = vec![0x81];
    v.extend(key(k));
    v.extend(prefix);
    v
}

/// (name, body, decodes to an error)
type Case = (&'static str, Vec<u8>, fn(&[u8]) -> bool);

const ARRAY32_MAX: [u8; 5] = [0xdd, 0xff, 0xff, 0xff, 0xff];
const MAP32_MAX: [u8; 5] = [0xdf, 0xff, 0xff, 0xff, 0xff];
const BIN32_MAX: [u8; 5] = [0xc6, 0xff, 0xff, 0xff, 0xff];
const STR32_MAX: [u8; 5] = [0xdb, 0xff, 0xff, 0xff, 0xff];

#[test]
fn huge_declared_lengths_fail_fast_without_allocating() {
    let before = vm_hwm_kib();
    let start = Instant::now();

    let cases: Vec<Case> = vec![
        ("ListReply.entries array32", one_field("entries", &ARRAY32_MAX), |b| {
            decode_body::<ListReply>(b).is_err()
        }),
        ("Hello.versions array32", one_field("versions", &ARRAY32_MAX), |b| {
            decode_body::<Hello>(b).is_err()
        }),
        ("GetReply.data bin32", one_field("data", &BIN32_MAX), |b| {
            decode_body::<GetReply>(b).is_err()
        }),
        ("GetReply.mime str32", one_field("mime", &STR32_MAX), |b| {
            decode_body::<GetReply>(b).is_err()
        }),
        ("List.query str32", one_field("query", &STR32_MAX), |b| decode_body::<List>(b).is_err()),
        ("top-level map32", MAP32_MAX.to_vec(), |b| decode_body::<ListReply>(b).is_err()),
        ("top-level array32", ARRAY32_MAX.to_vec(), |b| decode_body::<Hello>(b).is_err()),
        ("unknown field map32", one_field("zzz", &MAP32_MAX), |b| decode_body::<List>(b).is_err()),
        ("unknown field bin32", one_field("zzz", &BIN32_MAX), |b| decode_body::<List>(b).is_err()),
        ("unknown field array32", one_field("zzz", &ARRAY32_MAX), |b| {
            decode_body::<List>(b).is_err()
        }),
    ];
    for (name, body, is_err) in cases {
        let t = Instant::now();
        assert!(is_err(&body), "{name}: decoded Ok");
        let took = t.elapsed();
        eprintln!("{name}: {took:?}");
        assert!(took < Duration::from_millis(200), "{name}: took {took:?}");
    }

    let grew = vm_hwm_kib().saturating_sub(before);
    eprintln!("total {:?}, VmHWM grew {grew} KiB", start.elapsed());
    assert!(grew < 64 * 1024, "peak RSS grew {grew} KiB");
}
