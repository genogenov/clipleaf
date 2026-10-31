//! The daemon's IPC socket through the real `clipleafd` binary: where it listens, who may use
//! it, the handshake and the connection-level errors of PROTOCOL.md §3 and §5, the connection
//! cap, and that one client can't hold up another.
//!
//! Every request other than `Hello` is answered by a handler that isn't written yet, so these
//! tests keep to frames the connection itself answers: `Hello`, unknown request indexes, and
//! frames that end the connection.

mod common;

use std::{
    fs,
    io::{ErrorKind, Write},
    os::fd::AsFd,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, PermissionsExt},
        net::UnixListener,
    },
    path::Path,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};
use clipleaf_ipc::{
    client::{Client, error::ClientError},
    frame::frame_reader::FrameReader,
    protocol::{
        codec::decode_body,
        handshake::{ErrorCode, ErrorReply, HelloReply},
        header::{MAX_FRAME_LEN, MAX_REQUEST_LEN, MessageType},
        v1::models::Status,
    },
};
use clipleaf_test_common::{bins::Guarded, raw::RawCompositor};
use common::{ipc::*, *};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Starts `clipleafd` with this environment on a compositor that never answers, and returns
/// how it exited. A daemon that got past its IPC setup would connect and wait for the
/// compositor forever: that fails here after `WAIT`.
fn run_until_exit(state: &StateDir, runtime: Option<&Path>) -> ExitStatus {
    let comp = RawCompositor::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_clipleafd"));
    command
        .env_clear()
        .env("WAYLAND_DISPLAY", comp.path())
        .env("XDG_STATE_HOME", state.root())
        .env("CLIPLEAF_LOG", "off")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(runtime) = runtime {
        command.env("XDG_RUNTIME_DIR", runtime);
    }
    let mut child = Guarded::new(command.spawn().unwrap());
    let start = Instant::now();
    loop {
        if let Some(status) = child.child().try_wait().unwrap() {
            return status;
        }
        assert!(start.elapsed() < WAIT, "clipleafd kept running");
        thread::sleep(Duration::from_millis(1));
    }
}

/// A handshake with the reference client on a socket that is already listening; `None` if the
/// daemon closed the connection instead of answering.
fn try_handshake(daemon: &Daemon) -> Option<Client> {
    match Client::connect_handshake(daemon.socket(), 4096, 4096, Some(GUARD)) {
        Ok(client) => Some(client),
        Err(ClientError::EofClosed) => None,
        Err(ClientError::Io(e)) if matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::BrokenPipe) => None,
        Err(e) => panic!("handshake: {e}"),
    }
}

// ---- Handshake ----

#[test]
fn a_client_completes_the_handshake_and_learns_the_version_and_the_daemon() {
    let mut daemon = Daemon::start();
    // The daemon picks the version it speaks from the client's list, wherever it is in it.
    let mut conn = Conn::open(&mut daemon);
    conn.send(&frame(7, &hello(&[99, 1])));
    let reply = conn.recv_msg::<HelloReply>(7);
    assert_eq!(reply.version, 1);
    assert!(reply.features.is_empty(), "{:?}", reply.features);
    let name = reply.daemon.expect("the daemon names itself");
    assert!(name.starts_with("clipleafd "), "{name}");
    conn.assert_usable(8);

    Client::connect_handshake(daemon.socket(), 4096, 4096, Some(GUARD)).expect("handshake with the reference client");
}

#[test]
fn a_first_frame_other_than_hello_gets_bad_request_and_the_connection_is_closed() {
    let mut daemon = Daemon::start();
    // A valid request of a known type, and one of an unknown index: neither may come first.
    for first in [frame(5, &Status {}), raw_frame(UNKNOWN_REQUEST, 5, &[])] {
        let mut conn = Conn::open(&mut daemon);
        conn.send(&first);
        conn.recv_error(0, ErrorCode::BadRequest);
        conn.expect_eof();
    }
}

#[test]
fn a_second_hello_gets_bad_request_with_its_id_and_the_connection_stays_usable() {
    let mut daemon = Daemon::start();
    let mut conn = Conn::ready(&mut daemon);
    conn.send(&frame(2, &hello(&[1])));
    conn.recv_error(2, ErrorCode::BadRequest);
    conn.assert_usable(3);
}

#[test]
fn a_hello_without_a_common_version_gets_unsupported_version_and_the_connection_is_closed() {
    let mut daemon = Daemon::start();
    let mut conn = Conn::open(&mut daemon);
    conn.send(&frame(4, &hello(&[99])));
    conn.recv_error(0, ErrorCode::UnsupportedVersion);
    conn.expect_eof();
}

#[test]
fn a_hello_that_does_not_decode_gets_bad_request_and_the_connection_is_closed() {
    let mut daemon = Daemon::start();
    let mut conn = Conn::open(&mut daemon);
    // 0xc1 is the one byte MessagePack never uses.
    conn.send(&raw_frame(HELLO, 4, &[0xc1]));
    conn.recv_error(0, ErrorCode::BadRequest);
    conn.expect_eof();
}

// ---- Unknown and out-of-place frames ----

#[test]
fn an_unknown_request_gets_unknown_request_with_its_id_and_the_stream_stays_in_sync() {
    let mut daemon = Daemon::start();
    let mut conn = Conn::ready(&mut daemon);
    // Index 0 and an index past the defined ones, the second with a body the daemon must skip.
    conn.send(&raw_frame([1, 0], 10, &[]));
    conn.send(&raw_frame(UNKNOWN_REQUEST, 11, &[0xc1; 300]));
    conn.recv_error(10, ErrorCode::UnknownRequest);
    conn.recv_error(11, ErrorCode::UnknownRequest);
    conn.assert_usable(12);
}

#[test]
fn a_frame_that_is_not_a_request_gets_bad_request_and_the_connection_is_closed() {
    let mut daemon = Daemon::start();
    // An unknown category, a reply, an event and an error: a client only sends requests.
    for msg_type in [[9, 1], [2, 1], [4, 1], [3, 0]] {
        let mut conn = Conn::ready(&mut daemon);
        conn.send(&raw_frame(msg_type, 42, &[0x80]));
        conn.recv_error(0, ErrorCode::BadRequest);
        conn.expect_eof();
    }
}

#[test]
fn a_frame_over_64_kib_closes_the_connection_without_a_reply() {
    let mut daemon = Daemon::start();
    let mut conn = Conn::ready(&mut daemon);
    // The cap counts the header: a 64 KiB frame is still read whole (and answered).
    let max_body = MAX_REQUEST_LEN as usize - 12;
    conn.send(&raw_frame(UNKNOWN_REQUEST, 2, &vec![0; max_body]));
    conn.recv_error(2, ErrorCode::UnknownRequest);
    // One byte more: only the header is sent, the daemon must not wait for the body.
    conn.send(&raw_header(HELLO, 3, max_body as u32 + 1));
    conn.expect_eof();
}

// ---- Connections ----

#[test]
fn past_32_connections_a_new_one_is_closed_and_the_others_keep_working() {
    let mut daemon = Daemon::start();
    let mut conns: Vec<Conn> = (0..32).map(|_| Conn::ready(&mut daemon)).collect();

    let mut over = Conn::open(&mut daemon);
    over.expect_eof();
    for (i, conn) in conns.iter_mut().enumerate() {
        conn.assert_usable(100 + i as u32);
    }

    // A closed connection frees its place, once the daemon has noticed it.
    drop(conns.pop());
    let start = Instant::now();
    while try_handshake(&daemon).is_none() {
        assert!(start.elapsed() < WAIT, "a closed connection never freed its place");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_client_that_never_reads_its_replies_does_not_hold_up_another_client() {
    let mut daemon = Daemon::start();
    let mut flood = Conn::ready(&mut daemon);
    flood.stream.set_nonblocking(true).unwrap();

    // Requests without reading a reply, until the daemon stops reading them: its replies
    // pile up (unsent) to its 8 MiB limit, then the socket buffers fill.
    let first_id = 2u32;
    let mut next_id = first_id;
    let mut pending: Vec<u8> = Vec::new();
    let mut written = 0usize;
    loop {
        if pending.is_empty() {
            for _ in 0..1024 {
                pending.extend(raw_frame(UNKNOWN_REQUEST, next_id, &[]));
                next_id += 1;
            }
        }
        match flood.stream.write(&pending) {
            Ok(n) => {
                pending.drain(..n);
                written += n;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let mut fds = [PollFd::new(flood.stream.as_fd(), PollEvents::OUT)];
                if matches!(poll(&mut fds, Some(Duration::from_millis(200))).unwrap(), PollResult::Timeout) {
                    break; // the daemon stopped reading
                }
            }
            Err(e) => panic!("write: {e}"),
        }
        assert!(written < 32 << 20, "the daemon never stopped reading ({written} bytes of requests)");
    }

    // Another client is answered meanwhile.
    let mut other = Conn::ready(&mut daemon);
    other.assert_usable(2);

    // Nothing was lost: every request is answered, in order, once the flood client reads.
    let mut reader = FrameReader::new(1 << 16, MAX_FRAME_LEN as usize);
    let mut expected = first_id;
    while expected < next_id {
        let mut events = PollEvents::IN;
        if !pending.is_empty() {
            events |= PollEvents::OUT;
        }
        let mut fds = [PollFd::new(flood.stream.as_fd(), events)];
        assert!(
            matches!(poll(&mut fds, Some(GUARD)).unwrap(), PollResult::Ready(_)),
            "the daemon stopped answering at req_id {expected} of {next_id}"
        );
        let revents = fds[0].revents();
        if revents.has_any(PollEvents::OUT) {
            match flood.stream.write(&pending) {
                Ok(n) => drop(pending.drain(..n)),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => panic!("write: {e}"),
            }
        }
        if revents.has_any(PollEvents::IN | PollEvents::HUP) {
            match reader.fill_from(&mut flood.stream) {
                Ok(0) => panic!("clipleafd closed the connection at req_id {expected}"),
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => panic!("read: {e}"),
            }
            while let Some(res) = reader.next_frame() {
                let (h, body) = res.unwrap();
                assert_eq!((h.msg_type, h.req_id), (MessageType::Error, expected));
                let err: ErrorReply = decode_body(body).unwrap();
                assert_eq!(err.code, ErrorCode::UnknownRequest);
                expected += 1;
            }
        }
    }
    flood.stream.set_nonblocking(false).unwrap();
    flood.assert_usable(next_id);
}

// ---- The socket file ----

#[test]
fn a_stale_socket_or_file_at_the_socket_path_is_replaced() {
    let state = StateDir::new();
    for stale_socket in [true, false] {
        let runtime = RuntimeDir::new();
        fs::DirBuilder::new().mode(0o700).create(runtime.dir()).unwrap();
        if stale_socket {
            // What a killed daemon leaves: the file of a socket nobody listens on.
            drop(UnixListener::bind(runtime.socket()).unwrap());
        } else {
            fs::write(runtime.socket(), b"not a socket").unwrap();
        }
        let mut daemon = Daemon::start_with(&state, &runtime);
        let mut conn = Conn::ready(&mut daemon);
        conn.assert_usable(2);
        assert!(fs::metadata(runtime.socket()).unwrap().file_type().is_socket());
        daemon.stop();
    }
}

#[test]
fn the_socket_dir_is_private_and_the_socket_is_removed_when_the_daemon_exits() {
    let state = StateDir::new();
    let runtime = RuntimeDir::new();
    let mut daemon = Daemon::start_with(&state, &runtime);
    let mut conn = Conn::ready(&mut daemon);
    assert_eq!(mode(&runtime.dir()), 0o700);
    assert_eq!(mode(&runtime.dir().join("lock")), 0o600);
    assert!(fs::metadata(runtime.socket()).unwrap().file_type().is_socket());

    // The one way the daemon exits today: the compositor goes away. An open connection
    // doesn't keep it running, and it is closed.
    let status = daemon.hang_up();
    assert_eq!(status.code(), Some(1), "{status}");
    assert!(!runtime.socket().exists(), "the socket outlived the daemon");
    conn.expect_eof();
}

#[test]
fn a_second_daemon_on_the_same_runtime_dir_exits_with_an_error_and_the_first_keeps_its_socket() {
    let runtime = RuntimeDir::new();
    let mut first = Daemon::start_with(&StateDir::new(), &runtime);
    let _ = Conn::ready(&mut first);

    // A history of its own, so only the runtime dir is shared.
    let status = run_until_exit(&StateDir::new(), Some(runtime.root()));
    assert!(!status.success(), "{status}");

    let mut conn = Conn::open_path(runtime.socket().as_path());
    conn.handshake();
    conn.assert_usable(2);
}

#[test]
fn without_a_usable_runtime_dir_the_daemon_exits_with_an_error_and_creates_no_socket() {
    let state = StateDir::new();
    // Unset, and not an absolute path.
    for runtime in [None, Some(Path::new("relative/run"))] {
        let status = run_until_exit(&state, runtime);
        assert!(!status.success(), "{runtime:?}: {status}");
    }

    // A regular file, and a dir the daemon can't write to.
    let file = RuntimeDir::new();
    let not_a_dir = file.root().join("file");
    fs::write(&not_a_dir, b"").unwrap();
    let read_only = RuntimeDir::new();
    fs::set_permissions(read_only.root(), fs::Permissions::from_mode(0o500)).unwrap();
    for runtime in [not_a_dir.as_path(), read_only.root()] {
        let status = run_until_exit(&state, Some(runtime));
        assert!(!status.success(), "{}: {status}", runtime.display());
    }
    assert!(!read_only.dir().exists());
    fs::set_permissions(read_only.root(), fs::Permissions::from_mode(0o700)).unwrap();

    // A path that doesn't exist is a misconfiguration: nothing is created along it.
    let parent = RuntimeDir::new();
    let missing = parent.root().join("missing");
    let status = run_until_exit(&state, Some(&missing));
    assert!(!status.success(), "{}: {status}", missing.display());
    assert!(!missing.exists());
}
