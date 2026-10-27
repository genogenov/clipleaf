//! Pipes as payload channels, and EOF checks on them.

use std::{
    io::{self, PipeReader, Read},
    os::fd::OwnedFd,
};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

/// A payload channel: the reader stays with the test, the write end is handed to code under test.
pub fn channel() -> (PipeReader, OwnedFd) {
    let (reader, writer) = io::pipe().expect("pipe");
    (reader, writer.into())
}

/// Reads to EOF with a blocking read. Use this wherever another thread or a process may still be
/// holding a transient reference to the write end (relay-backed and process-spawning tests).
pub fn read_to_eof(reader: &mut PipeReader) -> Vec<u8> {
    let mut out = Vec::new();
    reader.read_to_end(&mut out).expect("read payload");
    out
}

/// Everything the reader will ever get, without waiting. Valid only when the writer wrote
/// synchronously on this thread and nothing else in the process scans fds or spawns processes
/// (a transient kernel reference would delay the EOF by microseconds, see
/// research/scm-eof-flake.md). `WouldBlock` before EOF means some copy of the write end is
/// still open: a leaked fd.
pub fn read_to_eof_now(reader: &mut PipeReader) -> Vec<u8> {
    let flags = fcntl_getfl(&*reader).expect("F_GETFL");
    fcntl_setfl(&*reader, flags | OFlags::NONBLOCK).expect("F_SETFL");
    let mut got = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return got,
            Ok(n) => got.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => panic!("write end still open: fd leaked (got {} bytes so far)", got.len()),
            Err(e) => panic!("reading the payload failed: {e}"),
        }
    }
}
