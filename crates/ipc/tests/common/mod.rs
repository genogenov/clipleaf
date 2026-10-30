//! Shared helpers for the clipleaf-ipc integration tests: temp dirs, a chunked reader,
//! a deterministic PRNG, sample messages and a scripted fake daemon on a Unix socket.

#![allow(dead_code)]

use std::{
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    thread::{self, JoinHandle},
    time::Duration,
};

use clipleaf_ipc::{
    client::Client,
    frame::frame_reader::FrameReader,
    protocol::{
        Message,
        codec::{decode_body, encode_frame},
        handshake::{Hello, HelloReply},
        header::{HEADER_LEN, Header, MAX_FRAME_LEN, MessageType},
        v1::models::*,
    },
};

/// Hang guard for every blocking socket read in the tests.
pub const GUARD: Duration = Duration::from_secs(2);

// ---- Temp dir ----

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
        // Short name: the socket path must fit in sun_path (108 bytes).
        let path = std::env::temp_dir().join(format!("cl-ipc-{}-{n}-{nanos}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---- Chunked reader ----

/// A `Read` that hands out `data` in the given chunk sizes (each read is also capped by the
/// caller's buffer), then the rest in one go, then EOF.
pub struct ChunkReader {
    data: Vec<u8>,
    pos: usize,
    chunks: Vec<usize>,
    next_chunk: usize,
}

impl ChunkReader {
    pub fn new(data: Vec<u8>, chunks: Vec<usize>) -> Self {
        Self {
            data,
            pos: 0,
            chunks,
            next_chunk: 0,
        }
    }

    pub fn exhausted(&self) -> bool {
        self.pos == self.data.len()
    }
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.data.len() - self.pos;
        // A 0 chunk would look like EOF; skip it (a split at offset 0 is no split).
        while self.chunks.get(self.next_chunk) == Some(&0) {
            self.next_chunk += 1;
        }
        let want = match self.chunks.get(self.next_chunk) {
            Some(&c) => {
                self.next_chunk += 1;
                c
            }
            None => remaining,
        };
        let n = want.min(remaining).min(buf.len());
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A frame copied out of a `FrameReader`.
#[derive(Debug, Clone)]
pub struct Frame {
    pub len: u32,
    pub msg_type: MessageType,
    pub nfds: u8,
    pub flags: u8,
    pub req_id: u32,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn from_parts(h: &Header, body: &[u8]) -> Self {
        Self {
            len: h.len,
            msg_type: h.msg_type,
            nfds: h.nfds,
            flags: h.flags,
            req_id: h.req_id,
            body: body.to_vec(),
        }
    }
}

/// Drains every frame from `read` until EOF; panics on a protocol error or trailing bytes.
pub fn read_all_frames(reader: &mut FrameReader, read: &mut impl Read) -> Vec<Frame> {
    let mut out = Vec::new();
    loop {
        while let Some(res) = reader.next_frame() {
            let (h, body) = res.expect("frame");
            out.push(Frame::from_parts(&h, body));
        }
        if reader.fill_from(read).unwrap() == 0 {
            return out;
        }
    }
}

// ---- Frame builders ----

pub fn frame<T: Message>(req_id: u32, msg: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_frame(req_id, msg, &mut buf, MAX_FRAME_LEN).unwrap();
    buf
}

/// A frame with an arbitrary header type; `body` is used verbatim.
pub fn raw_frame(msg_type: [u8; 2], req_id: u32, body: &[u8]) -> Vec<u8> {
    let header = Header {
        len: body.len() as u32,
        msg_type: MessageType::from_wire(msg_type[0], msg_type[1]),
        nfds: 0,
        flags: 0,
        req_id,
    };
    let mut buf = header.encode().to_vec();
    buf.extend_from_slice(body);
    buf
}

/// Body bytes of `msg` (the frame without its header).
pub fn body_of<T: Message>(msg: &T) -> Vec<u8> {
    frame(1, msg)[HEADER_LEN..].to_vec()
}

// ---- PRNG ----

pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next_u64() as u8).collect()
    }
}

// ---- Sample messages ----

pub fn sample_entry(id: u64) -> EntrySummary {
    EntrySummary {
        id,
        kind: Kind::Image,
        time_ms: 1_761_700_000_000 + id,
        pinned: id.is_multiple_of(2),
        size: 123_456,
        preview: vec![0, 1, 2, 0xff, b'a'],
        types: vec![
            TypeSummary {
                mime: "image/png".into(),
                size: 100_000,
            },
            TypeSummary {
                mime: "text/uri-list".into(),
                size: 23_456,
            },
        ],
        thumb: Some(Thumb {
            state: ThumbState::Ready,
            format: Some("image/jpeg".into()),
            width: Some(128),
            height: Some(96),
            src_width: Some(4000),
            src_height: Some(3000),
            orientation: Some(6),
            has_alpha: Some(false),
            data: Some(vec![0xff, 0xd8, 0xff, 0xe0, 0, 0x10]),
        }),
        file_count: Some(3),
        file_op: Some(FileOp::Cut),
        score: Some(0.75),
    }
}

// ---- Fake daemon ----

/// The daemon side of one connection.
pub struct Peer {
    pub stream: UnixStream,
    reader: FrameReader,
}

impl Peer {
    pub fn recv(&mut self) -> Frame {
        loop {
            if let Some(res) = self.reader.next_frame() {
                let (h, body) = res.expect("daemon: bad frame from client");
                return Frame::from_parts(&h, body);
            }
            let n = self.reader.fill_from(&mut self.stream).expect("daemon: read (hang guard?)");
            assert!(n > 0, "daemon: client closed the connection");
        }
    }

    /// Receives one frame and decodes it as `T`, checking the header type.
    pub fn recv_msg<T: Message>(&mut self) -> (u32, T) {
        let f = self.recv();
        assert_eq!(f.msg_type, T::TYPE, "daemon: unexpected frame type");
        assert_eq!(f.len as usize, f.body.len());
        (f.req_id, decode_body::<T>(&f.body).expect("daemon: decode"))
    }

    pub fn send<T: Message>(&mut self, req_id: u32, msg: &T) {
        self.send_raw(&frame(req_id, msg));
    }

    pub fn send_raw(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).expect("daemon: write");
    }

    /// Keeps the connection open until the client closes it (frames are discarded).
    pub fn wait_close(&mut self) {
        let mut buf = [0u8; 4096];
        while self.stream.read(&mut buf).expect("daemon: read (hang guard?)") > 0 {}
    }

    /// Answers the client's Hello with version 1 and returns that Hello and its req_id.
    pub fn handshake(&mut self) -> (u32, Hello) {
        self.handshake_with(1)
    }

    pub fn handshake_with(&mut self, version: u32) -> (u32, Hello) {
        let (req_id, hello) = self.recv_msg::<Hello>();
        self.send(
            req_id,
            &HelloReply {
                version,
                features: vec![],
                daemon: Some("fake 0.0.0".into()),
            },
        );
        (req_id, hello)
    }
}

/// A listening socket plus a thread that accepts one connection and runs `script` on it.
pub struct Daemon<R> {
    pub path: PathBuf,
    handle: Option<JoinHandle<R>>,
    _dir: TempDir,
}

impl<R: Send + 'static> Daemon<R> {
    pub fn spawn(script: impl FnOnce(Peer) -> R + Send + 'static) -> Self {
        let dir = TempDir::new();
        let path = dir.path().join("d.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(GUARD)).unwrap();
            stream.set_write_timeout(Some(GUARD)).unwrap();
            script(Peer {
                stream,
                reader: FrameReader::new(4096, MAX_FRAME_LEN as usize),
            })
        });
        Self {
            path,
            handle: Some(handle),
            _dir: dir,
        }
    }

    /// Waits for the script; re-raises a daemon-side panic.
    pub fn join(mut self) -> R {
        match self.handle.take().unwrap().join() {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    pub fn connect(&self) -> Client {
        Client::connect_handshake(&self.path, 4096, 4096, Some(GUARD)).expect("handshake")
    }
}
