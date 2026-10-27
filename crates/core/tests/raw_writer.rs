//! Request encoding at the 4096-byte write buffer's edges and with fds, observed on the wire by
//! the scripted raw peer: overflow at every position, oversized messages, the 28-fd rule (fds
//! counted per `recvmsg`, each paired with its own message), and a compositor that hangs up
//! mid-flush. This binary never scans fds or spawns processes, so non-blocking EOF checks are valid.

mod helpers;

use std::{
    fs::File,
    io::{self, ErrorKind, PipeReader, Write},
};

use clipleaf_core::{WlDataControlOffer, WlSessionManager};
use clipleaf_test_common::{
    fds,
    raw::{Batch, all_bytes, wire},
};
use helpers::*;

const BUF_LEN: usize = 4096;
/// The most fds one sendmsg may carry to libwayland.
const MAX_FDS_PER_SEND: usize = 28;
/// The id of the (made-up) compositor-side offer the `receive` requests go to.
const OFFER: u32 = SERVER_ID_START;

fn offer_size(mime: &str) -> usize {
    12 + (mime.len() + 1).next_multiple_of(4)
}

/// A session whose write buffer is empty, plus a data source whose `offer` requests are the
/// filler messages. `offer` is the source's request; `queued` counts the buffered bytes.
struct Writer<F> {
    rig: Rig,
    source: u32,
    offer: F,
    queued: usize,
    next_filler: usize,
}

fn writer() -> Writer<impl Fn(&mut WlSessionManager, &str) -> io::Result<()>> {
    let mut rig = rig();
    let source = rig.session.create_data_source().unwrap();
    let source_id = source.local_id;
    rig.flush_and_drain();
    Writer {
        rig,
        source: source_id,
        offer: move |s: &mut WlSessionManager, mime: &str| source.offer(s.get_message_writer(), mime),
        queued: 0,
        next_filler: 0,
    }
}

impl<F: Fn(&mut WlSessionManager, &str) -> io::Result<()>> Writer<F> {
    fn offer(&mut self, mime: &str) -> io::Result<()> {
        (self.offer)(&mut self.rig.session, mime)?;
        self.queued += offer_size(mime);
        Ok(())
    }

    fn receive(&mut self, mime: &str, fd: std::os::fd::OwnedFd) -> io::Result<()> {
        WlDataControlOffer::receive(OFFER, self.rig.session.get_message_writer(), mime, fd)?;
        self.queued += offer_size(mime); // same layout: header, length, padded text
        Ok(())
    }

    /// Queues whole `offer`s until exactly `target` bytes are buffered; returns their mimes.
    fn fill_to(&mut self, target: usize) -> Vec<String> {
        let mut mimes = Vec::new();
        while self.queued < target {
            let left = target - self.queued;
            assert!(left >= 16, "cannot fill {left} bytes with whole messages");
            // Never leave a remainder smaller than the smallest message (16 bytes).
            let size = match left {
                ..=100 => left,
                101..=115 => left - 16,
                _ => 100,
            };
            // 8 header + 4 length + padded string: a string of size-13 bytes needs no padding.
            // The mime is the filler's index in hex, zero padded, so every one is unique.
            let len = size - 13;
            let mime = format!("{:0>len$x}", self.next_filler);
            self.next_filler += 1;
            self.offer(&mime).unwrap();
            mimes.push(mime);
        }
        assert_eq!(self.queued, target);
        mimes
    }

    fn flush(&mut self) -> Vec<Batch> {
        self.queued = 0;
        self.rig.flush_batches()
    }

    /// The mime of every `offer` request of our source in the batches, in order.
    fn offered(&self, batches: &[Batch]) -> Vec<String> {
        wire::parse(&all_bytes(batches))
            .iter()
            .filter(|r| r.object_id == self.source)
            .map(|r| {
                assert_eq!(r.opcode, SOURCE_OFFER);
                String::from_utf8(r.string_at(0).0.to_vec()).unwrap()
            })
            .collect()
    }
}

/// Plays the compositor for `receive` requests: each fd gets the mime of its own message written
/// into it. Panics unless every batch carries exactly one fd per message.
fn answer_receives(batches: Vec<Batch>) {
    for batch in batches {
        let requests = wire::parse(&batch.bytes);
        let receives: Vec<_> = requests.iter().filter(|r| r.object_id == OFFER).collect();
        assert_eq!(receives.len(), batch.fds.len(), "fds and receives per recvmsg");
        for (request, fd) in receives.into_iter().zip(batch.fds) {
            assert_eq!(request.opcode, OFFER_RECEIVE);
            File::from(fd).write_all(request.string_at(0).0).unwrap();
        }
    }
}

fn read_all(mut reader: PipeReader) -> String {
    String::from_utf8(fds::read_to_eof_now(&mut reader)).unwrap()
}

fn fds_per_batch(batches: &[Batch]) -> Vec<usize> {
    batches.iter().map(|b| b.fds.len()).collect()
}

#[test]
fn a_message_that_exactly_fills_the_buffer_fits_without_a_flush() {
    let mut w = writer();
    // Header (8) + length prefix (4) + text + NUL == buffer length, no padding needed.
    let text = "x".repeat(BUF_LEN - 8 - 4 - 1);
    w.offer(&text).unwrap();
    assert!(w.rig.peer().recv_batches().is_empty(), "nothing should have been sent yet");
    let batches = w.flush();
    assert_eq!(w.offered(&batches), [text]);
}

#[test]
fn a_run_of_offers_larger_than_the_buffer_arrives_complete_and_in_order() {
    let mut w = writer();
    let mut expected = Vec::new();
    let mut total = 0;
    for i in 0..300u32 {
        // Varying lengths so the overflow lands on every kind of boundary.
        let mime = format!("application/x-{i}-{}", "k".repeat(i as usize % 53));
        total += offer_size(&mime);
        w.offer(&mime).unwrap();
        expected.push(mime);
    }
    assert!(total > 3 * BUF_LEN, "the run must overflow several times");
    let mut batches = w.rig.peer().recv_batches();
    batches.extend(w.flush());
    assert_eq!(w.offered(&batches), expected);
}

#[test]
fn overflow_at_every_position_keeps_the_message_intact() {
    // `remaining` is the room left when the message starts: below 8 not even the header fits,
    // and below 44 the string does not.
    let mime = format!("a/{}", "b".repeat(28));
    let message_len = offer_size(&mime);
    assert_eq!(message_len, 44);
    for remaining in (0..=64).step_by(4) {
        let mut w = writer();
        let mut expected = w.fill_to(BUF_LEN - remaining);
        w.offer(&mime).unwrap();
        expected.push(mime.clone());

        // On overflow the earlier messages are already on the wire (and only they); otherwise
        // nothing has been sent yet.
        let early = w.rig.peer().recv_batches();
        let on_wire = all_bytes(&early).len();
        let overflowed = remaining < message_len;
        assert_eq!(on_wire, if overflowed { BUF_LEN - remaining } else { 0 }, "remaining {remaining}");
        let mut batches = early;
        batches.extend(w.flush());
        assert_eq!(w.offered(&batches), expected, "remaining {remaining}");
    }
}

#[test]
fn a_message_larger_than_the_buffer_is_an_error_and_earlier_messages_still_arrive() {
    let mut w = writer();
    let (reader, fd) = fds::channel();
    w.offer("a/b").unwrap();
    w.receive("c/d", fd).unwrap();
    w.offer("e/f").unwrap();

    let err = w.offer(&"x".repeat(BUF_LEN + 1)).unwrap_err();
    assert!(err.to_string().contains("does not fit"), "{err}");

    // The writer is still usable and the oversized message left no trace.
    w.offer("g/h").unwrap();
    let mut batches = w.rig.peer().recv_batches();
    batches.extend(w.flush());
    assert_eq!(w.offered(&batches), ["a/b", "e/f", "g/h"]);
    assert_eq!(all_bytes(&batches).len(), 4 * 16);
    answer_receives(batches);
    assert_eq!(read_all(reader), "c/d");
}

#[test]
fn a_message_larger_than_the_buffer_with_nothing_before_it_sends_nothing() {
    let mut w = writer();
    let err = w.offer(&"x".repeat(BUF_LEN)).unwrap_err();
    assert!(err.to_string().contains("does not fit"), "{err}");
    assert!(w.flush().is_empty());

    w.offer("g/h").unwrap();
    let batches = w.flush();
    assert_eq!(w.offered(&batches), ["g/h"]);
}

#[test]
fn a_request_that_fails_before_it_is_committed_closes_its_fd() {
    let mut w = writer();
    let (reader, fd) = fds::channel();
    let err = w.receive(&"x".repeat(BUF_LEN), fd).unwrap_err();
    assert!(err.to_string().contains("does not fit"), "{err}");
    // EOF, not a hang: nobody holds the write end any more.
    assert_eq!(read_all(reader), "");
    assert!(w.flush().is_empty());
}

#[test]
fn committed_fds_are_closed_in_the_sender_once_sent() {
    let mut w = writer();
    let (reader, fd) = fds::channel();
    w.receive("c/d", fd).unwrap();
    let batches = w.flush();
    answer_receives(batches);
    assert_eq!(read_all(reader), "c/d");
}

fn receives(w: &mut Writer<impl Fn(&mut WlSessionManager, &str) -> io::Result<()>>, count: usize) -> Vec<PipeReader> {
    (0..count)
        .map(|i| {
            let (reader, fd) = fds::channel();
            w.receive(&format!("x/{i}"), fd).unwrap();
            reader
        })
        .collect()
}

#[test]
fn the_29th_fd_forces_a_flush_that_carries_the_first_28_with_their_messages() {
    let mut w = writer();
    let readers = receives(&mut w, MAX_FDS_PER_SEND + 1);
    // Only the 29th message is still queued, with its fd; the first 28 already went out.
    let mut batches = w.rig.peer().recv_batches();
    assert_eq!(fds_per_batch(&batches), [MAX_FDS_PER_SEND]);
    assert_eq!(wire::parse(&batches[0].bytes).len(), MAX_FDS_PER_SEND);

    batches.extend(w.flush());
    assert_eq!(fds_per_batch(&batches), [MAX_FDS_PER_SEND, 1]);
    answer_receives(batches);
    for (i, reader) in readers.into_iter().enumerate() {
        assert_eq!(read_all(reader), format!("x/{i}"), "message {i}");
    }
}

#[test]
fn no_send_carries_more_than_28_fds_and_every_fd_stays_with_its_own_message() {
    for count in [1, 27, 28, 29, 56, 57] {
        let mut w = writer();
        let readers = receives(&mut w, count);
        let mut batches = w.rig.peer().recv_batches();
        batches.extend(w.flush());

        let mut expected = vec![MAX_FDS_PER_SEND; count / MAX_FDS_PER_SEND];
        if count % MAX_FDS_PER_SEND != 0 {
            expected.push(count % MAX_FDS_PER_SEND);
        }
        assert_eq!(fds_per_batch(&batches), expected, "{count} receives");

        answer_receives(batches);
        for (i, reader) in readers.into_iter().enumerate() {
            assert_eq!(read_all(reader), format!("x/{i}"), "{count} receives, message {i}");
        }
    }
}

#[test]
fn an_overflow_flush_takes_the_fds_of_the_earlier_messages_and_leaves_later_ones() {
    let mut w = writer();
    let (first_reader, first_fd) = fds::channel();
    let (second_reader, second_fd) = fds::channel();

    w.receive("a/first", first_fd).unwrap();
    let mut expected = w.fill_to(BUF_LEN - 20);
    // Overflows in the middle: the receive and the fillers go out, with the first fd.
    let mime = "m".repeat(43);
    w.offer(&mime).unwrap();
    expected.push(mime);
    w.receive("a/second", second_fd).unwrap();

    let mut batches = w.rig.peer().recv_batches();
    assert_eq!(fds_per_batch(&batches), [1]);
    batches.extend(w.flush());
    assert_eq!(fds_per_batch(&batches), [1, 1]);
    assert_eq!(w.offered(&batches), expected);
    answer_receives(batches);
    assert_eq!(read_all(first_reader), "a/first");
    assert_eq!(read_all(second_reader), "a/second");
}

#[test]
fn a_compositor_that_hangs_up_during_an_overflow_flush_is_a_broken_pipe_and_the_fd_is_closed() {
    let mut w = writer();
    let (reader, fd) = fds::channel();
    w.receive("a/first", fd).unwrap();
    w.fill_to(BUF_LEN - 20);
    w.rig.hang_up();

    // Header written, the string does not fit, the flush hits a closed peer (EPIPE).
    let err = w.offer(&"m".repeat(43)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err}");
    // An Err ends the session; the fd still queued closes with it, not leaked.
    drop(w);
    assert_eq!(read_all(reader), "");
}

#[test]
fn a_compositor_that_hangs_up_during_a_commit_time_flush_closes_every_queued_fd() {
    let mut w = writer();
    let mut readers = receives(&mut w, MAX_FDS_PER_SEND);
    w.rig.hang_up();

    // The 29th fd does not fit, so the commit flushes first and the flush fails.
    let (reader, fd) = fds::channel();
    readers.push(reader);
    let err = w.receive("x/last", fd).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{err}");
    // The 29th fd closed on the way out; the queued 28 close with the session.
    drop(w);
    for reader in readers {
        assert_eq!(read_all(reader), "");
    }
}

#[test]
fn destroy_has_no_arguments_and_receive_carries_the_mime_and_the_fd() {
    let mut w = writer();
    let (reader, fd) = fds::channel();
    w.receive("text/plain", fd).unwrap();
    WlDataControlOffer::destroy(OFFER, w.rig.session.get_message_writer()).unwrap();
    let batches = w.flush();

    assert_eq!(fds_per_batch(&batches), [1]);
    let requests = wire::parse(&all_bytes(&batches));
    assert_eq!((requests[0].object_id, requests[0].opcode), (OFFER, OFFER_RECEIVE));
    assert_eq!(requests[0].string_at(0).0, b"text/plain");
    assert_eq!(
        requests[1],
        wire::Request {
            object_id: OFFER,
            opcode: OFFER_DESTROY,
            body: Vec::new()
        }
    );
    let mut fds_iter = batches.into_iter().flat_map(|b| b.fds);
    File::from(fds_iter.next().unwrap()).write_all(b"hello").unwrap();
    assert_eq!(read_all(reader), "hello");
}
