//! `serve_bytes_nonblocking` and `serve_file_nonblocking`: the daemon's serving, which gives up on a
//! paster that stops reading for longer than the stall timeout, and never on one that keeps
//! reading, however slowly. These need real time: the stalls are short (a few hundred ms), and
//! every serve runs on its own thread with a bound on the wait, so a hang fails instead of
//! hanging the suite.

use std::{
    env,
    fs::{self, File},
    io::{ErrorKind, Read},
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::{serve_bytes_nonblocking, serve_file_nonblocking};

/// Three times a grown 1 MiB pipe: serving has to wait for the reader many times over.
const LEN: usize = 3 << 20;
const STALL: Duration = Duration::from_millis(300);
/// Far longer than any serve here should take; only bounds a hang.
const BOUND: Duration = Duration::from_secs(20);

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// A payload file, removed when dropped.
struct Payload(std::path::PathBuf, File);
impl Payload {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = env::temp_dir().join(format!(
            "clipleaf-fd-io-nb-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        Self(path, file)
    }
}
impl Drop for Payload {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Runs one serve on its own thread; returns its result and how long it took.
fn serve_in_thread(
    serve: impl FnOnce() -> std::io::Result<()> + Send + 'static,
) -> (std::io::Result<()>, Duration) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let start = Instant::now();
        let result = serve();
        let _ = tx.send((result, start.elapsed()));
    });
    rx.recv_timeout(BOUND).expect("the serve hung")
}

/// Reads `chunk` bytes, then pauses `gap`, until EOF: slower than the writer, never silent
/// for a whole stall.
fn slow_reader(
    mut pipe: std::io::PipeReader,
    chunk: usize,
    gap: Duration,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            match pipe.read(&mut buf).unwrap() {
                0 => return out,
                n => out.extend_from_slice(&buf[..n]),
            }
            thread::sleep(gap);
        }
    })
}

#[test]
fn a_paster_that_never_reads_is_given_up_after_the_stall() {
    // Bytes, a payload file through a pipe (the two-stage splice), and a file into a socket (the
    // plain write loop): each one returns TimedOut once the reader has been silent for STALL.
    let bytes = data(LEN);
    let payload = Payload::new(&bytes);
    let file = payload.1.try_clone().unwrap();

    let (_reader, writer) = std::io::pipe().unwrap();
    let (result, took) =
        serve_in_thread(move || serve_bytes_nonblocking(OwnedFd::from(writer), &bytes, STALL));
    assert_eq!(result.unwrap_err().kind(), ErrorKind::TimedOut);
    assert!(took >= STALL && took < STALL * 5, "{took:?}");

    let (_reader, writer) = std::io::pipe().unwrap();
    let f = file.try_clone().unwrap();
    let (result, took) = serve_in_thread(move || {
        serve_file_nonblocking(OwnedFd::from(writer), &f, LEN as u64, STALL)
    });
    assert_eq!(result.unwrap_err().kind(), ErrorKind::TimedOut);
    assert!(took >= STALL && took < STALL * 5, "{took:?}");

    let (_peer, socket) = UnixStream::pair().unwrap();
    let (result, _) = serve_in_thread(move || {
        serve_file_nonblocking(OwnedFd::from(socket), &file, LEN as u64, STALL)
    });
    assert_eq!(result.unwrap_err().kind(), ErrorKind::TimedOut);
}

#[test]
fn a_slow_but_steady_reader_is_never_cut_off_and_gets_every_byte() {
    // 64 KiB every 50 ms: the whole paste takes several stalls' worth of time, but no gap is
    // anywhere near one. The file goes through the two-stage splice, which waits for the
    // paster many times here.
    let bytes = data(LEN);
    let payload = Payload::new(&bytes);
    let file = payload.1.try_clone().unwrap();

    let (reader, writer) = std::io::pipe().unwrap();
    let reading = slow_reader(reader, 64 * 1024, Duration::from_millis(50));
    let to_serve = bytes.clone();
    let (result, took) =
        serve_in_thread(move || serve_bytes_nonblocking(OwnedFd::from(writer), &to_serve, STALL));
    result.unwrap();
    assert!(
        took > STALL * 2,
        "the reader wasn't slow enough to test anything: {took:?}"
    );
    assert!(reading.join().unwrap() == bytes, "bytes differ");

    let (reader, writer) = std::io::pipe().unwrap();
    let reading = slow_reader(reader, 64 * 1024, Duration::from_millis(50));
    let (result, _) = serve_in_thread(move || {
        serve_file_nonblocking(OwnedFd::from(writer), &file, LEN as u64, STALL)
    });
    result.unwrap();
    assert!(reading.join().unwrap() == bytes, "bytes differ");
}

#[test]
fn a_paster_that_closes_its_end_ends_the_serve_with_broken_pipe() {
    let bytes = data(LEN);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let (result, _) =
        serve_in_thread(move || serve_bytes_nonblocking(OwnedFd::from(writer), &bytes, STALL));
    assert_eq!(result.unwrap_err().kind(), ErrorKind::BrokenPipe);
}

#[test]
fn a_small_payload_into_a_regular_file_is_written_whole() {
    // A regular file never blocks: the non-blocking path is a plain write there.
    let payload = Payload::new(b"");
    let out = File::options().write(true).open(&payload.0).unwrap();
    let (result, _) =
        serve_in_thread(move || serve_bytes_nonblocking(OwnedFd::from(out), b"hello, file", STALL));
    result.unwrap();
    let mut written = Vec::new();
    File::open(&payload.0)
        .unwrap()
        .read_to_end(&mut written)
        .unwrap();
    assert_eq!(written, b"hello, file");
}
