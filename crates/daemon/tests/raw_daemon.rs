//! The real `clipleafd` binary against the scripted raw peer: which copies it fetches, which
//! types it asks for, and that a source that never answers holds up nothing but its own job.
//!
//! No sleeps: every wait is for a request or a log line, bounded at 10 s only so a hang fails.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    os::fd::OwnedFd,
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use clipleaf_core::{PollFd, PollResult, ffi::POLLIN, poll};
use clipleaf_test_common::{
    bins::Guarded,
    raw::{
        self, RawCompositor, RawPeer,
        proto::*,
        wire::{self, Request},
    },
};

const WAIT: Duration = Duration::from_secs(10);
const UTF8: &str = "text/plain;charset=utf-8";
const OFFER_A: u32 = SERVER_ID_START;
const OFFER_B: u32 = SERVER_ID_START + 1;
const OFFER_C: u32 = SERVER_ID_START + 2;

/// A `receive` the daemon sent: which offer, which type, and the write end it wants filled.
struct Receive {
    offer: u32,
    mime: String,
    fd: OwnedFd,
}

struct Daemon {
    peer: RawPeer,
    lines: Receiver<String>,
    fds: VecDeque<OwnedFd>,
    child: Option<Guarded>,
    _comp: RawCompositor,
}

impl Daemon {
    /// Starts `clipleafd` on a fresh raw compositor and answers its registry round trip.
    fn start() -> Self {
        let comp = RawCompositor::new();
        let mut child = Command::new(env!("CARGO_BIN_EXE_clipleafd"))
            .env_clear()
            .env("WAYLAND_DISPLAY", comp.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clipleafd");
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let peer = comp.accept();
        peer.send(&raw::registry_reply(&[
            (1, "wl_seat", 1),
            (54, "ext_data_control_manager_v1", 1),
        ]));
        let daemon = Self {
            peer,
            lines,
            fds: VecDeque::new(),
            child: Some(Guarded::new(child)),
            _comp: comp,
        };
        // Device events only make sense once the device exists: wait for get_data_device.
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|r: &Request| r.object_id == MANAGER && r.opcode == MANAGER_GET_DATA_DEVICE)
        {
            daemon.wait_readable("get_data_device");
            seen.extend(daemon.peer.requests());
        }
        daemon
    }

    fn wait_readable(&self, what: &str) {
        let mut fds = [PollFd::new(self.peer.as_fd(), POLLIN)];
        let ready = poll(&mut fds, Some(WAIT)).unwrap();
        assert!(
            matches!(ready, PollResult::Ready(_)),
            "no {what} from clipleafd"
        );
    }

    fn send(&self, events: &[Vec<u8>]) {
        self.peer.send(&events.concat());
    }

    /// Every `receive` the daemon sends until (and including) one for `offer`.
    fn receives_until(&mut self, offer: u32) -> Vec<Receive> {
        let mut receives = Vec::new();
        loop {
            self.wait_readable(&format!("receive for {offer:#x}"));
            let batches = self.peer.recv_batches();
            assert!(!batches.is_empty(), "clipleafd hung up");
            for batch in batches {
                self.fds.extend(batch.fds);
                for request in wire::parse(&batch.bytes) {
                    if let Some(receive) = self.as_receive(&request) {
                        receives.push(receive);
                    }
                }
            }
            if receives.iter().any(|r| r.offer == offer) {
                return receives;
            }
        }
    }

    fn as_receive(&mut self, request: &Request) -> Option<Receive> {
        if request.object_id < SERVER_ID_START || request.opcode != OFFER_RECEIVE {
            return None;
        }
        let (mime, _) = request.string_at(0);
        Some(Receive {
            offer: request.object_id,
            mime: String::from_utf8(mime.to_vec()).unwrap(),
            fd: self.fds.pop_front().expect("a receive without its fd"),
        })
    }

    /// Waits for the next log line that contains `needle`.
    fn line_with(&self, needle: &str) -> String {
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(e) => panic!("no log line with {needle:?}: {e}"),
            }
        }
    }
}

fn offer(id: u32, mimes: &[&str]) -> Vec<u8> {
    let mut m = wire::msg(DEVICE, DEVICE_DATA_OFFER, &id.to_ne_bytes());
    for mime in mimes {
        m.extend(wire::msg(id, OFFER_OFFER, &wire::string(mime.as_bytes())));
    }
    m
}

fn selection(id: u32) -> Vec<u8> {
    wire::msg(DEVICE, DEVICE_SELECTION, &id.to_ne_bytes())
}

fn answer(receive: Receive, bytes: &[u8]) {
    std::fs::File::from(receive.fd).write_all(bytes).unwrap();
}

#[test]
fn a_copy_is_fetched_as_its_best_text_and_image_and_logged_with_their_sizes() {
    let mut daemon = Daemon::start();
    daemon.send(&[
        offer(
            OFFER_A,
            &["text/html", UTF8, "image/png", "application/x-foo"],
        ),
        selection(OFFER_A),
    ]);

    let receives = daemon.receives_until(OFFER_A);
    let mimes: Vec<&str> = receives.iter().map(|r| r.mime.as_str()).collect();
    assert_eq!(mimes, [UTF8, "image/png"]);
    let sizes = [5, 3];
    for (receive, size) in receives.into_iter().zip(sizes) {
        answer(receive, &vec![b'x'; size]);
    }
    daemon.line_with(&format!("[({UTF8:?}, 5), (\"image/png\", 3)]"));
}

#[test]
fn a_source_that_never_answers_does_not_hold_up_the_next_copy() {
    let mut daemon = Daemon::start();
    daemon.send(&[offer(OFFER_A, &[UTF8]), selection(OFFER_A)]);
    let stalled = daemon.receives_until(OFFER_A); // kept open, never written

    daemon.send(&[offer(OFFER_B, &[UTF8]), selection(OFFER_B)]);
    let mut receives = daemon.receives_until(OFFER_B);
    assert_eq!(receives.len(), 1);
    answer(receives.remove(0), b"b");
    daemon.line_with(&format!("[({UTF8:?}, 1)]"));
    drop(stalled);
}

#[test]
fn a_burst_of_copies_in_one_read_fetches_only_the_last() {
    let mut daemon = Daemon::start();
    daemon.send(&[
        offer(OFFER_A, &[UTF8]),
        selection(OFFER_A),
        offer(OFFER_B, &[UTF8]),
        selection(OFFER_B),
        offer(OFFER_C, &[UTF8]),
        selection(OFFER_C),
    ]);
    let offers: Vec<u32> = daemon
        .receives_until(OFFER_C)
        .iter()
        .map(|r| r.offer)
        .collect();
    assert_eq!(offers, [OFFER_C]);
}

#[test]
fn password_manager_copies_and_the_primary_selection_are_not_fetched() {
    let mut daemon = Daemon::start();
    daemon.send(&[
        offer(OFFER_A, &[UTF8, "x-kde-passwordManagerHint"]),
        selection(OFFER_A),
    ]);
    // Each change is logged before its fetch would start, so waiting for the line keeps the
    // three copies in separate drains (one drain would fetch only the last one anyway).
    daemon.line_with("Offered mime types: text/plain;charset=utf-8, x-kde-passwordManagerHint");
    daemon.send(&[
        offer(OFFER_B, &[UTF8]),
        wire::msg(DEVICE, DEVICE_PRIMARY_SELECTION, &OFFER_B.to_ne_bytes()),
    ]);
    daemon.line_with("Offered mime types: text/plain;charset=utf-8");
    // A later copy that is fetched: any receive for A or B would have been sent before it.
    daemon.send(&[offer(OFFER_C, &[UTF8]), selection(OFFER_C)]);
    let offers: Vec<u32> = daemon
        .receives_until(OFFER_C)
        .iter()
        .map(|r| r.offer)
        .collect();
    assert_eq!(offers, [OFFER_C]);
}

#[test]
fn the_daemon_exits_with_an_error_when_the_compositor_goes_away() {
    let mut daemon = Daemon::start();
    // One fetched copy first, so the hang-up comes while the loop is running, not during setup.
    daemon.send(&[offer(OFFER_A, &[UTF8]), selection(OFFER_A)]);
    let _pending = daemon.receives_until(OFFER_A);

    let Daemon { peer, child, .. } = daemon;
    peer.close();
    let status = child.unwrap().wait();
    assert_eq!(status.code(), Some(1), "{status}");
}
