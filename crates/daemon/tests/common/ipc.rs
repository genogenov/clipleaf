//! A raw client on the daemon's IPC socket: frames built and read byte for byte, so a test can
//! send what the reference client never would (no `Hello`, unknown types, oversized lengths).
//!
//! Every read blocks with a `GUARD` timeout, so a daemon that stays silent fails the test
//! instead of hanging it. The daemon spawns no processes, so its EOF is prompt: `expect_eof`
//! reads to EOF with a blocking read.

use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use clipleaf_ipc::{
    frame::frame_reader::FrameReader,
    protocol::{
        Message,
        codec::{decode_body, encode_frame},
        handshake::{ErrorCode, ErrorReply, Hello, HelloReply},
        header::{HEADER_LEN, MAX_FRAME_LEN, MessageType, RequestType},
    },
};

use super::{Daemon, WAIT};

/// Hang guard for every blocking read and write on an IPC socket.
pub const GUARD: Duration = Duration::from_secs(5);

/// A frame with its header fields.
#[derive(Debug)]
pub struct Frame {
    pub msg_type: MessageType,
    pub req_id: u32,
    pub body: Vec<u8>,
}

/// `msg` as a frame with this `req_id`.
pub fn frame<T: Message>(req_id: u32, msg: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_frame(req_id, msg, &mut buf, MAX_FRAME_LEN).unwrap();
    buf
}

/// A frame with any `[category, index]` and a verbatim body.
pub fn raw_frame(msg_type: [u8; 2], req_id: u32, body: &[u8]) -> Vec<u8> {
    raw_header(msg_type, req_id, body.len() as u32)
        .into_iter()
        .chain(body.iter().copied())
        .collect()
}

/// Only a header, announcing `len` bytes of body.
pub fn raw_header(msg_type: [u8; 2], req_id: u32, len: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER_LEN);
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&msg_type);
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&req_id.to_le_bytes());
    b
}

pub fn hello(versions: &[u32]) -> Hello {
    Hello {
        versions: versions.to_vec(),
        features: Vec::new(),
    }
}

/// Request category, an index no version defines.
pub const UNKNOWN_REQUEST: [u8; 2] = [1, 200];

/// Connects to the daemon's socket, waiting (bounded) for the daemon to bind it: the daemon
/// starts listening only after it is connected to the compositor. Fails if the daemon exits.
pub fn connect(daemon: &mut Daemon) -> UnixStream {
    let start = Instant::now();
    loop {
        match UnixStream::connect(daemon.socket()) {
            Ok(stream) => return stream,
            // Not bound yet, or a stale socket file still in place.
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {}
            Err(e) => panic!("connect {}: {e}", daemon.socket().display()),
        }
        if let Some(status) = daemon.exited() {
            panic!("clipleafd exited ({status}) before it listened on its socket");
        }
        assert!(start.elapsed() < WAIT, "clipleafd never listened on {}", daemon.socket().display());
        thread::sleep(Duration::from_millis(1));
    }
}

pub struct Conn {
    pub stream: UnixStream,
    reader: FrameReader,
}

impl Conn {
    pub fn new(stream: UnixStream) -> Self {
        stream.set_read_timeout(Some(GUARD)).unwrap();
        stream.set_write_timeout(Some(GUARD)).unwrap();
        Self {
            stream,
            reader: FrameReader::new(4096, MAX_FRAME_LEN as usize),
        }
    }

    /// A connection that hasn't sent anything yet.
    pub fn open(daemon: &mut Daemon) -> Self {
        Self::new(connect(daemon))
    }

    /// A connection to an already listening socket at `path`.
    pub fn open_path(path: &Path) -> Self {
        Self::new(UnixStream::connect(path).unwrap_or_else(|e| panic!("connect {}: {e}", path.display())))
    }

    /// A connection past the handshake (version 1, req_id 1).
    pub fn ready(daemon: &mut Daemon) -> Self {
        let mut conn = Self::open(daemon);
        conn.handshake();
        conn
    }

    /// Sends `Hello [1]` with req_id 1 and returns the reply.
    pub fn handshake(&mut self) -> HelloReply {
        self.send(&frame(1, &hello(&[1])));
        self.recv_msg::<HelloReply>(1)
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    /// The next frame; fails on EOF or after `GUARD` of silence.
    pub fn recv(&mut self) -> Frame {
        loop {
            if let Some(res) = self.reader.next_frame() {
                let (h, body) = res.expect("a frame from clipleafd");
                assert_eq!(h.len as usize, body.len());
                return Frame {
                    msg_type: h.msg_type,
                    req_id: h.req_id,
                    body: body.to_vec(),
                };
            }
            match self.reader.fill_from(&mut self.stream) {
                Ok(0) => panic!("clipleafd closed the connection, expected a frame"),
                Ok(_) => {}
                Err(e) => panic!("no frame from clipleafd: {e}"),
            }
        }
    }

    /// The next frame, which must be a `T` with this `req_id`.
    pub fn recv_msg<T: Message>(&mut self, req_id: u32) -> T {
        let f = self.recv();
        if f.msg_type == MessageType::Error && T::TYPE != MessageType::Error {
            panic!("expected {:?}, got {:?}", T::TYPE, decode_body::<ErrorReply>(&f.body));
        }
        assert_eq!(f.msg_type, T::TYPE);
        assert_eq!(f.req_id, req_id);
        decode_body::<T>(&f.body).unwrap()
    }

    /// The next frame, which must be an `Error` with this code and `req_id`.
    pub fn recv_error(&mut self, req_id: u32, code: ErrorCode) {
        let err = self.recv_msg::<ErrorReply>(req_id);
        assert_eq!(err.code, code, "{err}");
    }

    /// The daemon closed the connection without sending anything more.
    pub fn expect_eof(&mut self) {
        assert!(self.reader.next_frame().is_none(), "a frame before EOF");
        let mut buf = [0u8; 4096];
        match self.stream.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => panic!("expected EOF, got {n} more bytes"),
            Err(e) => panic!("expected EOF: {e}"),
        }
    }

    /// The connection still answers: an unknown request gets `unknown_request` with its id.
    pub fn assert_usable(&mut self, req_id: u32) {
        self.send(&raw_frame(UNKNOWN_REQUEST, req_id, &[]));
        self.recv_error(req_id, ErrorCode::UnknownRequest);
    }
}

/// The request type `Hello` as a wire pair, for building frames by hand.
pub const HELLO: [u8; 2] = [1, RequestType::Hello as u8];
