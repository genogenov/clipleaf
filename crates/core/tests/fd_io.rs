//! `serve_bytes` and `serve_file`: what reaches the paster's fd, however it is opened.
//! No sleeps and no timeouts: readers block on their pipes. This binary never scans fds or
//! spawns processes, so the non-blocking EOF checks are valid.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, PipeReader, Read, Seek},
    os::{
        fd::OwnedFd,
        unix::fs::{FileExt, OpenOptionsExt},
    },
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use clipleaf_core::{serve_bytes, serve_file};
use clipleaf_test_common::{
    fds,
    rustix::{
        fs::{OFlags, fcntl_getfl, fcntl_setfl},
        io::ioctl_fionread,
        pipe::{fcntl_getpipe_size, fcntl_setpipe_size},
    },
};

// Larger than a grown 1 MiB pipe, so serving has to wait for the reader.
const LEN: usize = 3 << 20;

type Serve<'a> = Box<dyn Fn(OwnedFd) -> io::Result<()> + Send + Sync + 'a>;

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn unique_temp_path() -> std::path::PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    env::temp_dir().join(format!(
        "clipleaf-fd-io-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

// Nameless like the payload files copy and the daemon serve.
fn temp_file() -> File {
    let path = unique_temp_path();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    fs::remove_file(&path).unwrap();
    file
}

fn file_with(data: &[u8]) -> File {
    let file = temp_file();
    file.write_all_at(data, 0).unwrap();
    file
}

fn contents(file: &File) -> Vec<u8> {
    let mut out = vec![0; file.metadata().unwrap().len() as usize];
    file.read_exact_at(&mut out, 0).unwrap();
    out
}

// The same bytes served both ways: from memory and from a file.
fn servers(expected: &[u8]) -> [(&'static str, Serve<'_>); 2] {
    let file = file_with(expected);
    [
        ("bytes", Box::new(move |out| serve_bytes(out, expected))),
        (
            "file",
            Box::new(move |out| serve_file(out, &file, expected.len() as u64)),
        ),
    ]
}

// Drains the pipe on another thread so serving never blocks for good.
fn pipe_reader() -> (OwnedFd, thread::JoinHandle<Vec<u8>>) {
    let (mut reader, writer) = io::pipe().unwrap();
    let reading = thread::spawn(move || {
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        out
    });
    (writer.into(), reading)
}

// assert_eq! would print megabytes of bytes on failure.
fn assert_same(actual: &[u8], expected: &[u8], what: &str) {
    let first_diff = actual.iter().zip(expected).position(|(a, b)| a != b);
    assert!(
        actual == expected,
        "{what}: got {} bytes, expected {}, first difference at {first_diff:?}",
        actual.len(),
        expected.len()
    );
}

#[test]
fn everything_arrives_through_a_pipe() {
    for len in [0, 1, 4096, 4097, LEN] {
        let expected = data(len);
        for (what, serve) in servers(&expected) {
            let (out, reading) = pipe_reader();
            serve(out).unwrap();
            assert_same(
                &reading.join().unwrap(),
                &expected,
                &format!("{what}, len {len}"),
            );
        }
    }
}

#[test]
fn everything_arrives_in_a_regular_file() {
    let expected = data(LEN);
    for (what, serve) in servers(&expected) {
        let out = temp_file();
        serve(out.try_clone().unwrap().into()).unwrap();
        assert_same(&contents(&out), &expected, what);
    }
}

#[test]
fn a_non_blocking_paste_pipe_whose_reader_has_not_started_still_gets_everything() {
    let expected = data(LEN);
    for (what, serve) in servers(&expected) {
        let (mut reader, writer) = io::pipe().unwrap();
        let flags = fcntl_getfl(&writer).unwrap();
        fcntl_setfl(&writer, flags | OFlags::NONBLOCK).unwrap();

        thread::scope(|s| {
            let serving = s.spawn(move || serve(writer.into()));
            // Do not read until the pipe is full: a write that stayed non-blocking would fail
            // with EAGAIN by now. This waits on pipe state, not on a clock.
            while !serving.is_finished() {
                // Read the fill level first and the capacity second: the capacity never shrinks,
                // so queued >= capacity proves the pipe was full. Works for any pipe size.
                let queued = ioctl_fionread(&reader).unwrap();
                if queued >= fcntl_getpipe_size(&reader).unwrap() as u64 {
                    break;
                }
                thread::yield_now();
            }
            let mut got = Vec::new();
            reader.read_to_end(&mut got).unwrap();
            serving.join().unwrap().unwrap();
            assert_same(&got, &expected, what);
        });
    }
}

#[test]
fn clearing_o_nonblock_keeps_the_pasters_other_flags() {
    let path = unique_temp_path();
    let out = OpenOptions::new()
        .append(true)
        .create_new(true)
        .custom_flags(OFlags::NONBLOCK.bits() as i32)
        .open(&path)
        .unwrap();
    fs::remove_file(&path).unwrap();
    let before = fcntl_getfl(&out).unwrap();
    assert!(before.contains(OFlags::NONBLOCK));
    assert!(before.contains(OFlags::APPEND));

    serve_bytes(out.try_clone().unwrap().into(), b"x").unwrap();

    // The clone shares the open file description, so its flags.
    assert_eq!(fcntl_getfl(&out).unwrap(), before & !OFlags::NONBLOCK);
}

#[test]
fn a_reader_that_closes_early_ends_the_paste_with_broken_pipe() {
    let expected = data(LEN);
    for (what, serve) in servers(&expected) {
        let (mut reader, writer) = io::pipe().unwrap();
        let reading = thread::spawn(move || {
            let mut first = [0u8; 1024];
            reader.read_exact(&mut first).unwrap();
        });
        let err = serve(writer.into()).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::BrokenPipe, "{what}: {err}");
        reading.join().unwrap();
    }
}

#[test]
fn the_paste_pipe_is_grown_to_1_mib_only_for_payloads_over_a_page() {
    // Unprivileged pipes can't exceed 1 MiB, so the never-shrink branch can't be exercised here.
    for (len, start, expected) in [
        (4096, 64 << 10, 64 << 10),
        (4097, 64 << 10, 1 << 20),
        (4097, 1 << 20, 1 << 20),
    ] {
        let (mut reader, writer) = io::pipe().unwrap();
        let probe = reader.try_clone().unwrap();
        fcntl_setpipe_size(&writer, start).unwrap();
        let reading = thread::spawn(move || {
            let mut out = Vec::new();
            reader.read_to_end(&mut out).unwrap();
            out.len()
        });
        serve_bytes(writer.into(), &data(len)).unwrap();
        assert_eq!(reading.join().unwrap(), len);
        let size = fcntl_getpipe_size(&probe).unwrap();
        assert_eq!(size, expected, "len {len}, started at {start}");
    }
}

#[test]
fn serving_closes_the_paste_fd() {
    // Per-fd check instead of counting the process's fds: after serving, the only open write end
    // would be a leaked one, and then the reader could not see EOF. The payload fits even an
    // ungrown 64 KiB pipe, so no reader thread is needed.
    for len in [0, 1, 4097, 60_000] {
        let expected = data(len);
        for (what, serve) in servers(&expected) {
            let (mut reader, writer): (PipeReader, OwnedFd) = fds::channel();
            serve(writer).unwrap();
            assert_same(
                &fds::read_to_eof_now(&mut reader),
                &expected,
                &format!("{what}, len {len}"),
            );
        }
    }
}

#[test]
fn serving_never_touches_the_payloads_file_cursor() {
    // Concurrent serves of one file are only safe if none of them reads or moves the shared cursor.
    let expected = data(LEN);
    let mut file = file_with(&expected);
    file.seek(io::SeekFrom::Start(1000)).unwrap();

    let (out, reading) = pipe_reader();
    serve_file(out, &file, LEN as u64).unwrap();
    assert_same(&reading.join().unwrap(), &expected, "pipe");
    let out = temp_file();
    serve_file(out.try_clone().unwrap().into(), &file, LEN as u64).unwrap();
    assert_same(&contents(&out), &expected, "regular file");

    assert_eq!(file.stream_position().unwrap(), 1000);
}

#[test]
fn threads_serving_one_file_at_once_all_get_identical_bytes() {
    let expected = data(LEN);
    let file = file_with(&expected);
    thread::scope(|s| {
        for thread_index in 0..4 {
            let (file, expected) = (&file, &expected);
            s.spawn(move || {
                for round in 0..5 {
                    let what = format!("thread {thread_index}, round {round}");
                    if thread_index % 2 == 0 {
                        let (out, reading) = pipe_reader();
                        serve_file(out, file, LEN as u64).unwrap();
                        assert_same(&reading.join().unwrap(), expected, &what);
                    } else {
                        let out = temp_file();
                        serve_file(out.try_clone().unwrap().into(), file, LEN as u64).unwrap();
                        assert_same(&contents(&out), expected, &what);
                    }
                }
            });
        }
    });
}
