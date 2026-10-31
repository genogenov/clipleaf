//! The real `clipleafd` binary against the scripted raw peer, shared by the daemon's tests.
//!
//! The tests observe only what the daemon does: the requests it sends and what it writes to
//! `wal.log` and `payloads/`, never its log lines. Every wait is for a request (blocking on the
//! socket) or for the next WAL record (the file is re-read every millisecond), bounded at 10 s
//! only so a hang fails. The state dir can outlive one daemon, so a test can stop it (SIGKILL,
//! the same as a crash) and start the next one on the same history.
//!
//! The history itself isn't readable until step 4's IPC, so a test asks it by copying: a copy
//! of an entry that is in the history is a duplicate (a Touch record), anything else is a new
//! entry (an Insert record). Set `CLIPLEAF_TEST_LOG=debug` to see the daemon's stderr.
//!
//! Every image type gets a Thumb record some time after its Insert, from one of two workers in
//! any order, so `next_record` sets Thumb records aside (`thumbs`) and `wait_thumb` waits for
//! the one of a given entry.
//!
//! When the daemon restores an entry it becomes the selection owner: the peer records the data
//! sources it creates (`create_data_source`, `offer`, `destroy`, answered with `delete_id` like a
//! compositor) and every `set_selection`, and a test pastes from a source with `paste`.
#![allow(dead_code)] // each test file uses a different part

pub mod ipc;
pub mod wal;

use std::{
    collections::VecDeque,
    fs,
    io::{Read, Write},
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::DirBuilderExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};
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

/// `$XDG_STATE_HOME` for one test: the daemon keeps its history in `<root>/clipleaf`.
/// Removed when the test ends.
pub struct StateDir(PathBuf);

impl StateDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("clipleafd-state-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
    /// Appends `total` bytes of records of an unknown type (dead bytes: the replay skips them,
    /// a compaction drops them), in pieces well under the 256 KiB record limit.
    pub fn append_dead(&self, mut total: u64) {
        const RECORD_OVERHEAD: u64 = 4 + 1 + 8; // len + type + checksum
        let salt = wal::salt_of(&self.wal_bytes());
        let mut file = fs::OpenOptions::new().append(true).open(self.wal()).unwrap();
        while total > 0 {
            let piece = total.min(200 * 1024);
            let body = vec![0u8; (piece - RECORD_OVERHEAD) as usize];
            file.write_all(&wal::record(salt, 99, &body)).unwrap();
            total -= piece;
        }
    }

    pub fn payload_files(&self) -> Vec<(String, u64)> {
        let mut files: Vec<_> = fs::read_dir(self.payloads())
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.file_name().into_string().unwrap(), e.metadata().unwrap().len())
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

/// `$XDG_RUNTIME_DIR` for one test (mode 0700, as a login session's): the daemon keeps its
/// IPC socket and runtime lock in `<root>/clipleaf`. Removed when the test ends.
pub struct RuntimeDir(PathBuf);

impl RuntimeDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        // Short: the socket path must fit in sun_path (108 bytes).
        let root = std::env::temp_dir().join(format!("clipleafd-run-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&root);
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }

    pub fn root(&self) -> &Path {
        &self.0
    }

    /// `<root>/clipleaf`, the daemon's own dir.
    pub fn dir(&self) -> PathBuf {
        self.0.join("clipleaf")
    }

    pub fn socket(&self) -> PathBuf {
        self.0.join(clipleaf_ipc::RELATIVE_SOCKET_PATH)
    }
}

impl Drop for RuntimeDir {
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

/// A data source the daemon created to own a selection, with the types it offered.
#[derive(Debug, Clone)]
pub struct Source {
    pub id: u32,
    pub mimes: Vec<String>,
    pub destroyed: bool,
}

/// A `set_selection` (or `set_primary_selection`) the daemon sent; `source` 0 is NULL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SetSelection {
    pub primary: bool,
    pub source: u32,
}

/// A `receive` the daemon sent: which offer, which type, and the write end it wants filled.
pub struct Receive {
    pub offer: u32,
    pub mime: String,
    pub fd: OwnedFd,
}

pub struct Daemon {
    peer: RawPeer,
    wal: PathBuf,
    /// WAL records already returned (or there at startup).
    seen: usize,
    /// Thumb records `next_record` stepped over, in log order: `(id, type index, thumb)`.
    thumbs: Vec<(u64, u32, Option<wal::Thumb>)>,
    /// Offers the daemon has destroyed so far.
    destroyed: Vec<u32>,
    /// Data sources the daemon created, in order.
    sources: Vec<Source>,
    /// Every `set_selection` / `set_primary_selection`, in order, and how many a test consumed.
    sets: Vec<SetSelection>,
    sets_seen: usize,
    fds: VecDeque<OwnedFd>,
    child: Option<Guarded>,
    next_offer: u32,
    socket: PathBuf,
    _comp: RawCompositor,
    _own_state: Option<StateDir>,
    _own_runtime: Option<RuntimeDir>,
}

impl Daemon {
    /// Starts `clipleafd` with a fresh history of its own.
    pub fn start() -> Self {
        let state = StateDir::new();
        let mut daemon = Self::start_in(&state);
        daemon._own_state = Some(state);
        daemon
    }

    /// Starts `clipleafd` on a fresh raw compositor with the history in `state` and a runtime
    /// dir of its own, and answers its registry round trip.
    pub fn start_in(state: &StateDir) -> Self {
        let runtime = RuntimeDir::new();
        let mut daemon = Self::start_with(state, &runtime);
        daemon._own_runtime = Some(runtime);
        daemon
    }

    /// As `start_in`, with `$XDG_RUNTIME_DIR` = `runtime`.
    pub fn start_with(state: &StateDir, runtime: &RuntimeDir) -> Self {
        let comp = RawCompositor::new();
        let mut command = Command::new(env!("CARGO_BIN_EXE_clipleafd"));
        command
            .env_clear()
            .env("WAYLAND_DISPLAY", comp.path())
            .env("XDG_STATE_HOME", state.root())
            .env("XDG_RUNTIME_DIR", runtime.root())
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        match std::env::var("CLIPLEAF_TEST_LOG") {
            Ok(level) => command.env("CLIPLEAF_LOG", level).stderr(Stdio::inherit()),
            Err(_) => command.env("CLIPLEAF_LOG", "off").stderr(Stdio::null()),
        };
        let mut child = command.spawn().expect("spawn clipleafd");
        // A daemon that fails at startup never connects: fail with its exit status, don't hang.
        let mut listener = [PollFd::new(comp.listener_fd(), PollEvents::IN)];
        if !matches!(poll(&mut listener, Some(WAIT)), Ok(PollResult::Ready(_))) {
            let _ = child.kill();
            panic!("clipleafd never connected: {:?}", child.wait());
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("clipleafd exited at startup: {status}");
        }
        let peer = comp.accept();
        peer.send(&raw::registry_reply(&[(1, "wl_seat", 1), (54, "ext_data_control_manager_v1", 1)]));
        let mut daemon = Self {
            peer,
            wal: state.wal(),
            seen: 0,
            thumbs: Vec::new(),
            destroyed: Vec::new(),
            sources: Vec::new(),
            sets: Vec::new(),
            sets_seen: 0,
            fds: VecDeque::new(),
            child: Some(Guarded::new(child)),
            next_offer: SERVER_ID_START,
            socket: runtime.socket(),
            _comp: comp,
            _own_state: None,
            _own_runtime: None,
        };
        // Device events only make sense once the device exists: wait for get_data_device.
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|r: &Request| r.object_id == MANAGER && r.opcode == MANAGER_GET_DATA_DEVICE)
        {
            daemon.wait_readable("get_data_device");
            let requests = daemon.peer.requests();
            // Readable with nothing to read is EOF: the daemon hung up (it failed at startup).
            if requests.is_empty() {
                let status = daemon.child.as_mut().unwrap().child().try_wait().unwrap();
                panic!("clipleafd hung up before get_data_device (exit status: {status:?})");
            }
            seen.extend(requests);
        }
        // The history is restored (and the log cut or reset) before the daemon connects.
        daemon.seen = wal::records(&fs::read(&daemon.wal).unwrap()).len();
        daemon
    }

    pub fn pid(&mut self) -> u32 {
        self.child.as_mut().unwrap().child().id()
    }

    /// Thumb records seen so far, in log order: `(id, type index, thumb)`.
    pub fn thumbs(&self) -> &[(u64, u32, Option<wal::Thumb>)] {
        &self.thumbs
    }

    /// Waits for the Thumb record of entry `id` (its only image type): the thumbnail, or `None`
    /// when the image can't get one.
    pub fn wait_thumb(&mut self, id: u64) -> Option<wal::Thumb> {
        loop {
            if let Some((_, _, thumb)) = self.thumbs.iter().find(|(i, _, _)| *i == id) {
                return thumb.clone();
            }
            match self.next_any_record() {
                wal::Record::Thumb { id, type_index, thumb } => self.thumbs.push((id, type_index, thumb)),
                other => panic!("expected only Thumb records while waiting for one, got {other:?}"),
            }
        }
    }

    /// Kills the daemon (no clean shutdown: what it wrote is what a crash leaves) and reaps it.
    pub fn stop(mut self) {
        drop(self.child.take());
    }

    /// Kills the compositor connection and returns how the daemon exited (within `WAIT`).
    pub fn hang_up(self) -> std::process::ExitStatus {
        let Daemon { peer, child, .. } = self;
        peer.close();
        let mut child = child.unwrap();
        let start = Instant::now();
        loop {
            if let Some(status) = child.child().try_wait().unwrap() {
                return status;
            }
            assert!(start.elapsed() < WAIT, "clipleafd kept running after the compositor went away");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The daemon's IPC socket path (`$XDG_RUNTIME_DIR/clipleaf/daemon.sock`).
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// How the daemon exited, if it has.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.as_mut().unwrap().child().try_wait().unwrap()
    }

    pub fn wait_readable(&self, what: &str) {
        let mut fds = [PollFd::new(self.peer.as_fd(), PollEvents::IN)];
        let ready = poll(&mut fds, Some(WAIT)).unwrap();
        assert!(matches!(ready, PollResult::Ready(_)), "no {what} from clipleafd");
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

    /// Every `receive` the daemon sends until it destroys `offer`. The daemon destroys the
    /// previous selection's offer while it reads the next selection, so this returns once that
    /// selection has been read.
    pub fn receives_until_destroyed(&mut self, offer: u32) -> Vec<Receive> {
        let mut receives = Vec::new();
        while !self.destroyed.contains(&offer) {
            receives.extend(self.receives_once());
        }
        receives
    }

    fn receives_while(&mut self, mut more: impl FnMut(&[Receive]) -> bool) -> Vec<Receive> {
        let mut receives = Vec::new();
        while more(&receives) {
            let got = self.receives_once();
            receives.extend(got);
        }
        receives
    }

    /// Reads one round of requests: the receives in it (destroys go to `self.destroyed`).
    fn receives_once(&mut self) -> Vec<Receive> {
        let mut receives = Vec::new();
        self.wait_readable("request");
        let batches = self.peer.recv_batches();
        assert!(!batches.is_empty(), "clipleafd hung up");
        for batch in batches {
            self.fds.extend(batch.fds);
            for request in wire::parse(&batch.bytes) {
                if request.object_id >= SERVER_ID_START && request.opcode == OFFER_DESTROY {
                    self.destroyed.push(request.object_id);
                }
                self.record_source_side(&request);
                if let Some(receive) = self.as_receive(&request) {
                    receives.push(receive);
                }
            }
        }
        receives
    }

    /// The owner side: sources created, offered, destroyed, and selections set.
    fn record_source_side(&mut self, request: &Request) {
        let (id, op) = (request.object_id, request.opcode);
        if id == MANAGER && op == MANAGER_CREATE_DATA_SOURCE {
            self.sources.push(Source {
                id: request.u32_at(0),
                mimes: Vec::new(),
                destroyed: false,
            });
        } else if id == DEVICE && (op == DEVICE_SET_SELECTION || op == DEVICE_SET_PRIMARY_SELECTION) {
            self.sets.push(SetSelection {
                primary: op == DEVICE_SET_PRIMARY_SELECTION,
                source: request.u32_at(0),
            });
        } else if id < SERVER_ID_START
            && let Some(source) = self.sources.iter_mut().rev().find(|s| s.id == id && !s.destroyed)
        {
            match op {
                SOURCE_OFFER => {
                    let (mime, _) = request.string_at(0);
                    source.mimes.push(String::from_utf8(mime.to_vec()).unwrap());
                }
                SOURCE_DESTROY => {
                    source.destroyed = true;
                    // As a compositor does: the id can be reused after this.
                    self.peer.send(&wire::msg(DISPLAY, DISPLAY_DELETE_ID, &id.to_ne_bytes()));
                }
                _ => {}
            }
        }
    }

    /// Reads requests until `done` holds (bounded by `WAIT`). Receives that arrive meanwhile are
    /// kept for later `receives_*` calls only through their fds; tests that wait on the owner
    /// side don't copy at the same time.
    pub fn pump_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let start = Instant::now();
        while !done(self) {
            assert!(start.elapsed() < WAIT, "clipleafd never: {what}");
            let _ = self.receives_once();
        }
    }

    /// Waits for the next `set_selection` / `set_primary_selection` the daemon sends.
    pub fn next_set_selection(&mut self) -> SetSelection {
        self.pump_until("set a selection", |d| d.sets.len() > d.sets_seen);
        self.sets_seen += 1;
        self.sets[self.sets_seen - 1]
    }

    /// Waits for the daemon to take the clipboard with a new source and returns it (its offers
    /// come before the `set_selection`, in the same flush).
    pub fn wait_restore(&mut self) -> Source {
        let set = self.next_set_selection();
        assert!(!set.primary && set.source != 0, "{set:?}");
        self.source(set.source)
    }

    /// The newest source with this id.
    pub fn source(&self, id: u32) -> Source {
        self.sources.iter().rev().find(|s| s.id == id).cloned().expect("a source with that id")
    }

    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Every selection set so far, including the ones already consumed.
    pub fn sets(&self) -> &[SetSelection] {
        &self.sets
    }

    /// Waits until the daemon destroys this source.
    pub fn wait_source_destroyed(&mut self, id: u32) {
        self.pump_until("destroyed its source", |d| d.sources.iter().any(|s| s.id == id && s.destroyed));
    }

    /// Asks the daemon's source for `mime`, as a pasting app does, and returns the read end.
    pub fn paste_start(&self, source: u32, mime: &[u8]) -> fs::File {
        let (rx, tx) = std::io::pipe().unwrap();
        self.peer
            .send_with_fds(&wire::msg(source, SOURCE_SEND, &wire::string(mime)), &[tx.as_fd()]);
        drop(tx); // the daemon holds the only write end now
        fs::File::from(OwnedFd::from(rx))
    }

    /// Events in one `sendmsg`, with `fds` riding on it (so the daemon reads them together).
    pub fn send_with_fds(&self, events: &[Vec<u8>], fds: &[std::os::fd::BorrowedFd<'_>]) {
        self.peer.send_with_fds(&events.concat(), fds);
    }

    /// One whole paste: everything the daemon writes until it closes the pipe.
    pub fn paste(&self, source: u32, mime: &[u8]) -> Vec<u8> {
        read_to_eof(self.paste_start(source, mime))
    }

    /// Tells the daemon's source it lost the selection.
    pub fn cancel(&self, source: u32) {
        self.send(&[wire::msg(source, SOURCE_CANCELLED, &[])]);
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
        let receives = self.receives_while(|rs| rs.iter().filter(|r| r.offer == id).count() < types.len());
        for receive in receives {
            assert_eq!(receive.offer, id, "a receive for an older copy");
            let (_, bytes) = types.iter().find(|(m, _)| *m == receive.mime).unwrap();
            answer(receive, bytes);
        }
    }

    /// A copy that must become a new entry: returns its Insert record.
    pub fn copy_new(&mut self, types: &[(&str, &[u8])]) -> wal::Insert {
        self.copy(types);
        self.next_insert()
    }

    /// A copy that must match an entry in the history: returns that entry's id.
    pub fn copy_dup(&mut self, types: &[(&str, &[u8])]) -> u64 {
        self.copy(types);
        match self.next_record() {
            wal::Record::Touch { id, .. } => id,
            other => panic!("expected a Touch (a duplicate), got {other:?}"),
        }
    }

    /// Whether these types are an entry in the history: copies them (so a missing one is
    /// added). Ask about missing ones last.
    pub fn has(&mut self, types: &[(&str, &[u8])]) -> bool {
        self.copy(types);
        match self.next_record() {
            wal::Record::Touch { .. } => true,
            wal::Record::Insert(_) => false,
            other => panic!("expected a Touch or an Insert, got {other:?}"),
        }
    }

    pub fn next_insert(&mut self) -> wal::Insert {
        match self.next_record() {
            wal::Record::Insert(insert) => insert,
            other => panic!("expected an Insert, got {other:?}"),
        }
    }

    /// The id of the next Delete record (an eviction).
    pub fn next_delete(&mut self) -> u64 {
        match self.next_record() {
            wal::Record::Delete { id } => id,
            other => panic!("expected a Delete, got {other:?}"),
        }
    }

    /// Waits until a compaction has replaced `wal.log` (its salt is no longer `old_salt`) and
    /// returns the records of the new log; `next_record` continues after them.
    pub fn wait_compacted(&mut self, old_salt: u64) -> Vec<wal::Record> {
        let start = Instant::now();
        loop {
            let bytes = fs::read(&self.wal).unwrap();
            if bytes.len() >= wal::HEADER_LEN && wal::salt_of(&bytes) != old_salt {
                let records = wal::records(&bytes);
                self.seen = records.len();
                return records;
            }
            let child = self.child.as_mut().unwrap().child();
            if let Some(status) = child.try_wait().unwrap() {
                panic!("clipleafd exited ({status}) before compacting");
            }
            assert!(start.elapsed() < WAIT, "clipleafd never compacted");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Waits for the next record the daemon writes to `wal.log` that isn't a Thumb record.
    pub fn next_record(&mut self) -> wal::Record {
        loop {
            match self.next_any_record() {
                wal::Record::Thumb { id, type_index, thumb } => self.thumbs.push((id, type_index, thumb)),
                record => return record,
            }
        }
    }

    /// Waits for the next record the daemon writes to `wal.log`, of any type.
    fn next_any_record(&mut self) -> wal::Record {
        let start = Instant::now();
        loop {
            let records = wal::records(&fs::read(&self.wal).unwrap());
            if let Some(record) = records.into_iter().nth(self.seen) {
                self.seen += 1;
                return record;
            }
            let child = self.child.as_mut().unwrap().child();
            if let Some(status) = child.try_wait().unwrap() {
                panic!("clipleafd exited ({status}) before writing record {}", self.seen);
            }
            assert!(start.elapsed() < WAIT, "no WAL record {} from clipleafd", self.seen);
            thread::sleep(Duration::from_millis(1));
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

/// The clipboard (or primary) is now empty: the owner left or cleared it.
pub fn cleared() -> Vec<u8> {
    selection(0)
}

/// Reads until EOF, waiting at most `WAIT` for each chunk.
pub fn read_to_eof(mut pipe: fs::File) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let mut fds = [PollFd::new(pipe.as_fd(), PollEvents::IN)];
        assert!(
            matches!(poll(&mut fds, Some(WAIT)), Ok(PollResult::Ready(_))),
            "the paste stalled after {} bytes",
            out.len()
        );
        match pipe.read(&mut buf).unwrap() {
            0 => return out,
            n => out.extend_from_slice(&buf[..n]),
        }
    }
}

pub fn answer(receive: Receive, bytes: &[u8]) {
    fs::File::from(receive.fd).write_all(bytes).unwrap();
}
