//! A scripted byte-level Wayland peer for fault injection.
//!
//! It plays the compositor with exact control over bytes and file descriptors: split messages,
//! EOF mid-message, malformed input, fds counted per `recvmsg`, non-UTF-8 strings. It has its own
//! wire encoder and decoder on rustix and shares nothing with `clipleaf_core::ffi`, so a wrong
//! constant or layout cannot be wrong on both sides.
//!
//! Unix sockets deliver a `sendmsg` synchronously, so after [`RawPeer::send`] returns the bytes
//! are readable by the client and after the client's `write` returns they are readable here:
//! nothing needs a sleep. The peer is driven from the test thread only.

use std::{
    io::{IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

/// Object ids and opcodes of the protocol as written in the XML (not taken from clipleaf).
pub mod proto {
    pub const DISPLAY: u32 = 1;
    /// First ids clipleaf hands out, in order: registry, then the sync callback.
    pub const REGISTRY: u32 = 2;
    pub const CALLBACK: u32 = 3;
    /// After `initialize`: manager bound first, then the seat, then the device.
    pub const MANAGER: u32 = 4;
    pub const SEAT: u32 = 5;
    pub const DEVICE: u32 = 6;
    /// Compositor-created objects live from here up.
    pub const SERVER_ID_START: u32 = 0xff00_0000;

    // wl_display
    pub const DISPLAY_ERROR: u16 = 0;
    pub const DISPLAY_DELETE_ID: u16 = 1;
    pub const DISPLAY_SYNC: u16 = 0;
    pub const DISPLAY_GET_REGISTRY: u16 = 1;
    // wl_registry
    pub const REGISTRY_GLOBAL: u16 = 0;
    pub const REGISTRY_BIND: u16 = 0;
    // wl_callback
    pub const CALLBACK_DONE: u16 = 0;
    // ext_data_control_manager_v1
    pub const MANAGER_CREATE_DATA_SOURCE: u16 = 0;
    pub const MANAGER_GET_DATA_DEVICE: u16 = 1;
    // ext_data_control_device_v1 events
    pub const DEVICE_DATA_OFFER: u16 = 0;
    pub const DEVICE_SELECTION: u16 = 1;
    pub const DEVICE_FINISHED: u16 = 2;
    pub const DEVICE_PRIMARY_SELECTION: u16 = 3;
    // ext_data_control_device_v1 requests
    pub const DEVICE_SET_SELECTION: u16 = 0;
    pub const DEVICE_SET_PRIMARY_SELECTION: u16 = 2;
    // ext_data_control_source_v1
    pub const SOURCE_OFFER: u16 = 0;
    pub const SOURCE_DESTROY: u16 = 1;
    pub const SOURCE_SEND: u16 = 0;
    pub const SOURCE_CANCELLED: u16 = 1;
    // ext_data_control_offer_v1
    pub const OFFER_RECEIVE: u16 = 0;
    pub const OFFER_DESTROY: u16 = 1;
    pub const OFFER_OFFER: u16 = 0;
}

/// Wire encoding and decoding (native endian, 4-byte aligned).
pub mod wire {
    /// One message: object id, then `size << 16 | opcode`, then the body.
    pub fn msg(object_id: u32, opcode: u16, body: &[u8]) -> Vec<u8> {
        let size = (8 + body.len()) as u32;
        let mut m = Vec::with_capacity(8 + body.len());
        m.extend_from_slice(&object_id.to_ne_bytes());
        m.extend_from_slice(&(size << 16 | u32::from(opcode)).to_ne_bytes());
        m.extend_from_slice(body);
        m
    }

    /// A string argument: length with NUL, bytes, NUL, zero padding to 4 bytes.
    pub fn string(s: &[u8]) -> Vec<u8> {
        let mut out = ((s.len() + 1) as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(s);
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    pub fn u32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    /// A decoded request.
    #[derive(Debug, PartialEq, Eq, Clone)]
    pub struct Request {
        pub object_id: u32,
        pub opcode: u16,
        pub body: Vec<u8>,
    }

    impl Request {
        pub fn u32_at(&self, offset: usize) -> u32 {
            u32::from_ne_bytes(self.body[offset..offset + 4].try_into().expect("u32"))
        }

        /// The string argument starting at `offset`, and the offset after it.
        pub fn string_at(&self, offset: usize) -> (&[u8], usize) {
            let len = self.u32_at(offset) as usize;
            let start = offset + 4;
            let bytes = &self.body[start..start + len - 1];
            assert_eq!(
                self.body[start + len - 1],
                0,
                "string is not NUL terminated"
            );
            (bytes, start + len.next_multiple_of(4))
        }
    }

    /// Splits bytes into whole requests. Panics on a torn or malformed message: what the client
    /// sent must always be well formed.
    pub fn parse(bytes: &[u8]) -> Vec<Request> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let header = rest.first_chunk::<8>().expect("torn header");
            let object_id = u32::from_ne_bytes(header[..4].try_into().unwrap());
            let word = u32::from_ne_bytes(header[4..].try_into().unwrap());
            let size = (word >> 16) as usize;
            assert!(
                size >= 8 && size.is_multiple_of(4) && size <= rest.len(),
                "bad or torn message (object {object_id}, size {size}, {} bytes left)",
                rest.len()
            );
            out.push(Request {
                object_id,
                opcode: word as u16,
                body: rest[8..size].to_vec(),
            });
            rest = &rest[size..];
        }
        out
    }
}

/// What one `recvmsg` returned.
pub struct Batch {
    pub bytes: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

/// A listening socket in a private dir under `temp_dir()` (a long path would exceed SUN_LEN).
pub struct RawCompositor {
    dir: PathBuf,
    path: PathBuf,
    listener: UnixListener,
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

impl RawCompositor {
    pub fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let dir = loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("clipleaf-raw-{}-{n}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => break dir,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create {}: {e}", dir.display()),
            }
        };
        let path = dir.join("wayland-raw");
        let listener = UnixListener::bind(&path).expect("bind");
        Self {
            dir,
            path,
            listener,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The listening socket, readable once a client is waiting in the backlog: poll it before
    /// [`Self::accept`] when the client may die before it connects.
    pub fn listener_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }

    /// Accepts the next connection. Blocking, but a client that has already connected is queued
    /// in the backlog, so it returns at once.
    pub fn accept(&self) -> RawPeer {
        let (sock, _) = self.listener.accept().expect("accept");
        RawPeer { sock }
    }
}

impl Default for RawCompositor {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RawCompositor {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The compositor's end of one connection.
pub struct RawPeer {
    sock: UnixStream,
}

impl RawPeer {
    /// One `sendmsg` with these bytes.
    pub fn send(&self, bytes: &[u8]) {
        self.send_with_fds(bytes, &[]);
    }

    /// One `sendmsg` with these bytes and fds (the fds ride with the first byte).
    pub fn send_with_fds(&self, bytes: &[u8], fds: &[BorrowedFd<'_>]) {
        let mut space =
            vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(fds.len().max(1)))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() {
            assert!(control.push(SendAncillaryMessage::ScmRights(fds)));
        }
        let sent = sendmsg(
            &self.sock,
            &[IoSlice::new(bytes)],
            &mut control,
            SendFlags::NOSIGNAL,
        )
        .expect("sendmsg");
        assert_eq!(sent, bytes.len(), "short sendmsg");
    }

    /// Stops this side's sending, so the client reads EOF after what was already sent. The
    /// client can still send requests.
    pub fn shutdown_write(&self) {
        self.sock
            .shutdown(std::net::Shutdown::Write)
            .expect("shutdown");
    }

    /// Closes the connection.
    pub fn close(self) {}

    /// Everything the client has sent so far, one entry per `recvmsg` (so the fds of each
    /// `sendmsg` are counted separately). Non-blocking: stops at "nothing more" or EOF.
    /// Panics if the kernel truncated the control data.
    pub fn recv_batches(&self) -> Vec<Batch> {
        let mut batches = Vec::new();
        loop {
            let mut buf = vec![0u8; 64 * 1024];
            // Room for far more fds than any sane client sends, so a violation is visible.
            let mut space = vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(253))];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let result = recvmsg(
                &self.sock,
                &mut [IoSliceMut::new(&mut buf)],
                &mut control,
                RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT,
            );
            let received = match result {
                Ok(r) => r,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => panic!("recvmsg: {e}"),
            };
            assert!(
                !received.flags.contains(ReturnFlags::CTRUNC),
                "control data truncated"
            );
            let mut fds = Vec::new();
            for message in control.drain() {
                if let RecvAncillaryMessage::ScmRights(received_fds) = message {
                    fds.extend(received_fds);
                }
            }
            if received.bytes == 0 {
                break; // EOF
            }
            buf.truncate(received.bytes);
            batches.push(Batch { bytes: buf, fds });
        }
        batches
    }

    /// All requests the client has sent so far (their fds are dropped).
    pub fn requests(&self) -> Vec<wire::Request> {
        wire::parse(&all_bytes(&self.recv_batches()))
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.sock.as_fd()
    }
}

/// The bytes of all batches, concatenated.
pub fn all_bytes(batches: &[Batch]) -> Vec<u8> {
    batches.iter().flat_map(|b| b.bytes.clone()).collect()
}

/// What a compositor sends right after `get_registry` + `sync`: one `global` per entry
/// (name, interface, version), then the callback's `done`.
pub fn registry_reply(globals: &[(u32, &str, u32)]) -> Vec<u8> {
    let mut events = Vec::new();
    for &(name, interface, version) in globals {
        let mut body = name.to_ne_bytes().to_vec();
        body.extend(wire::string(interface.as_bytes()));
        body.extend(version.to_ne_bytes());
        events.extend(wire::msg(proto::REGISTRY, proto::REGISTRY_GLOBAL, &body));
    }
    events.extend(callback_done(proto::CALLBACK));
    events
}

pub fn callback_done(callback: u32) -> Vec<u8> {
    wire::msg(callback, proto::CALLBACK_DONE, &0u32.to_ne_bytes())
}
