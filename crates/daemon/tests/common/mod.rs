//! The real `clipleafd` binary against the scripted raw peer, shared by the daemon's tests.
//!
//! No sleeps: every wait is for a request or a log line, bounded at 10 s only so a hang fails.
//! The state dir can outlive one daemon, so a test can stop it (SIGKILL, the same as a crash)
//! and start the next one on the same history.
#![allow(dead_code)] // each test file uses a different part

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver},
    },
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

pub const WAIT: Duration = Duration::from_secs(10);
pub const UTF8: &str = "text/plain;charset=utf-8";
pub const PNG: &str = "image/png";
/// Logged by the log thread once a record is written (debug level).
pub const APPENDED: &str = "Processed WAL append";
pub const DELETED: &str = "Processed WAL delete";
pub const DUPLICATE: &str = "Copy matches entry";

/// `$XDG_STATE_HOME` for one test: the daemon keeps its history in `<root>/clipleaf`.
/// Removed when the test ends.
pub struct StateDir(PathBuf);

impl StateDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "clipleafd-state-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    pub fn root(&self) -> &Path {
        &self.0
    }

    pub fn wal(&self) -> PathBuf {
        self.0.join("clipleaf/wal.log")
    }

    pub fn payloads(&self) -> PathBuf {
        self.0.join("clipleaf/payloads")
    }

    pub fn wal_bytes(&self) -> Vec<u8> {
        fs::read(self.wal()).unwrap()
    }

    pub fn wal_len(&self) -> u64 {
        fs::metadata(self.wal()).unwrap().len()
    }

    /// `(name, size)` of every entry in `payloads/`, sorted by entry id, then type number.
    pub fn payload_files(&self) -> Vec<(String, u64)> {
        let mut files: Vec<_> = fs::read_dir(self.payloads())
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().into_string().unwrap(),
                    e.metadata().unwrap().len(),
                )
            })
            .collect();
        files.sort_by_key(|(name, _)| id_and_type(name));
        files
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `<id>.<n>` → `(id, n)`; anything else sorts last.
pub fn id_and_type(name: &str) -> (u64, u64) {
    name.split_once('.')
        .and_then(|(id, n)| Some((id.parse().ok()?, n.parse().ok()?)))
        .unwrap_or((u64::MAX, u64::MAX))
}

/// A `receive` the daemon sent: which offer, which type, and the write end it wants filled.
pub struct Receive {
    pub offer: u32,
    pub mime: String,
    pub fd: OwnedFd,
}

pub struct Daemon {
    peer: RawPeer,
    lines: Receiver<String>,
    fds: VecDeque<OwnedFd>,
    child: Option<Guarded>,
    next_offer: u32,
    _comp: RawCompositor,
    _own_state: Option<StateDir>,
}

impl Daemon {
    /// Starts `clipleafd` with a fresh history of its own.
    pub fn start() -> Self {
        let state = StateDir::new();
        let mut daemon = Self::start_in(&state);
        daemon._own_state = Some(state);
        daemon
    }

    /// Starts `clipleafd` on a fresh raw compositor with the history in `state`, and answers
    /// its registry round trip.
    pub fn start_in(state: &StateDir) -> Self {
        let comp = RawCompositor::new();
        let mut child = Command::new(env!("CARGO_BIN_EXE_clipleafd"))
            .env_clear()
            .env("WAYLAND_DISPLAY", comp.path())
            .env("XDG_STATE_HOME", state.root())
            .env("CLIPLEAF_LOG", "debug") // the log lines the tests wait for
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
        // A daemon that fails at startup never connects: fail with its exit status, don't hang.
        let mut listener = [PollFd::new(comp.listener_fd(), POLLIN)];
        if !matches!(poll(&mut listener, Some(WAIT)), Ok(PollResult::Ready(_))) {
            let _ = child.kill();
            panic!("clipleafd never connected: {:?}", child.wait());
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("clipleafd exited at startup: {status}");
        }
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
            next_offer: SERVER_ID_START,
            _comp: comp,
            _own_state: None,
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

    /// Kills the daemon (no clean shutdown: what it wrote is what a crash leaves) and reaps it.
    pub fn stop(mut self) {
        drop(self.child.take());
    }

    /// Kills the compositor connection and returns how the daemon exited.
    pub fn hang_up(self) -> std::process::ExitStatus {
        let Daemon { peer, child, .. } = self;
        peer.close();
        child.unwrap().wait()
    }

    pub fn wait_readable(&self, what: &str) {
        let mut fds = [PollFd::new(self.peer.as_fd(), POLLIN)];
        let ready = poll(&mut fds, Some(WAIT)).unwrap();
        assert!(
            matches!(ready, PollResult::Ready(_)),
            "no {what} from clipleafd"
        );
    }

    pub fn send(&self, events: &[Vec<u8>]) {
        self.peer.send(&events.concat());
    }

    /// A new offer id for the next copy.
    pub fn new_offer(&mut self) -> u32 {
        let id = self.next_offer;
        self.next_offer += 1;
        id
    }

    /// Every `receive` the daemon sends until (and including) one for `offer`.
    pub fn receives_until(&mut self, offer: u32) -> Vec<Receive> {
        self.receives_while(|rs| !rs.iter().any(|r| r.offer == offer))
    }

    fn receives_while(&mut self, mut more: impl FnMut(&[Receive]) -> bool) -> Vec<Receive> {
        let mut receives = Vec::new();
        while more(&receives) {
            self.wait_readable("receive");
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
        }
        receives
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

    /// One copy: offers exactly these types (each one is fetched: at most one text and one
    /// image), then plays the source, answering every receive with that type's bytes.
    pub fn copy(&mut self, types: &[(&str, &[u8])]) {
        let id = self.new_offer();
        let mimes: Vec<&str> = types.iter().map(|(m, _)| *m).collect();
        self.send(&[offer(id, &mimes), selection(id)]);
        let receives =
            self.receives_while(|rs| rs.iter().filter(|r| r.offer == id).count() < types.len());
        for receive in receives {
            assert_eq!(receive.offer, id, "a receive for an older copy");
            let (_, bytes) = types.iter().find(|(m, _)| *m == receive.mime).unwrap();
            answer(receive, bytes);
        }
    }

    /// A copy that must become a new entry: returns once its record is written.
    pub fn copy_new(&mut self, types: &[(&str, &[u8])]) {
        self.copy(types);
        self.line_with(APPENDED);
    }

    /// Waits until each needle has appeared in some line, in any order (lines from different
    /// threads, such as a job's result and its WAL append, may come either way round).
    pub fn lines_with(&self, needles: &[&str]) {
        let mut missing: Vec<&str> = needles.to_vec();
        while !missing.is_empty() {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) => missing.retain(|n| !line.contains(n)),
                Err(e) => panic!("no log lines with {missing:?}: {e}"),
            }
        }
    }

    /// Waits for the next log line that contains `needle`.
    pub fn line_with(&self, needle: &str) -> String {
        loop {
            match self.lines.recv_timeout(WAIT) {
                Ok(line) if line.contains(needle) => return line,
                Ok(_) => {}
                Err(e) => panic!("no log line with {needle:?}: {e}"),
            }
        }
    }
}

pub fn offer(id: u32, mimes: &[&str]) -> Vec<u8> {
    let mut m = wire::msg(DEVICE, DEVICE_DATA_OFFER, &id.to_ne_bytes());
    for mime in mimes {
        m.extend(wire::msg(id, OFFER_OFFER, &wire::string(mime.as_bytes())));
    }
    m
}

pub fn selection(id: u32) -> Vec<u8> {
    wire::msg(DEVICE, DEVICE_SELECTION, &id.to_ne_bytes())
}

pub fn answer(receive: Receive, bytes: &[u8]) {
    fs::File::from(receive.fd).write_all(bytes).unwrap();
}
