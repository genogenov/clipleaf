use std::io::{self, Write};
use std::{
    fs::File,
    io::ErrorKind,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::{FileExt, FileTypeExt},
    },
    ptr,
};

use crate::ffi::{F_GETFL, O_NONBLOCK};
use crate::{
    ffi::{F_GETPIPE_SZ, F_SETFL, F_SETPIPE_SZ, fcntl, splice},
    log_debug,
};

pub const FD_BUFFER_LEN: usize = 32;

pub struct InFdBuffer {
    in_fds: [Option<OwnedFd>; FD_BUFFER_LEN],
    in_fd_count: usize,
    in_fds_cursor: usize,
}

pub struct OutFdBuffer {
    out_fds: [Option<OwnedFd>; FD_BUFFER_LEN],
    out_fd_count: usize,
}

impl InFdBuffer {
    pub fn new() -> Self {
        Self {
            in_fds: [const { None }; FD_BUFFER_LEN],
            in_fd_count: 0,
            in_fds_cursor: 0,
        }
    }

    pub fn pop_last_in_fd(&mut self) -> Option<OwnedFd> {
        // this is a ring buffer, so we need to wrap around if we reach the end of the buffer
        if self.in_fd_count == 0 {
            return None;
        }
        let fd = self.in_fds[self.in_fds_cursor].take();
        self.in_fds_cursor = (self.in_fds_cursor + 1) % self.in_fds.len();
        self.in_fd_count -= 1;
        fd
    }

    pub fn push_in_fd(&mut self, fd: OwnedFd) -> std::io::Result<()> {
        // this is a ring buffer, so we need to wrap around if we reach the end of the buffer
        if self.in_fd_count >= self.in_fds.len() {
            return Err(std::io::Error::other(
                "Not enough space in buffer for file descriptors",
            ));
        }
        self.in_fds[(self.in_fds_cursor + self.in_fd_count) % self.in_fds.len()] = Some(fd);
        self.in_fd_count += 1;
        Ok(())
    }
}

impl OutFdBuffer {
    pub fn new() -> Self {
        Self {
            out_fds: [const { None }; FD_BUFFER_LEN],
            out_fd_count: 0,
        }
    }

    pub fn push_out_fd(&mut self, fd: OwnedFd) -> std::io::Result<()> {
        if self.out_fd_count >= self.out_fds.len() {
            return Err(std::io::Error::other(
                "Not enough space in output buffer for file descriptors",
            ));
        }
        self.out_fds[self.out_fd_count] = Some(fd);
        self.out_fd_count += 1;
        Ok(())
    }

    pub fn peek_out_fds(&self) -> &[Option<OwnedFd>] {
        &self.out_fds[..self.out_fd_count]
    }

    pub fn truncate_out(&mut self, truncate_count: usize) {
        if truncate_count > self.out_fd_count {
            return;
        }
        for slot in &mut self.out_fds[self.out_fd_count - truncate_count..self.out_fd_count] {
            *slot = None;
        }
        self.out_fd_count -= truncate_count;
    }

    pub fn clear_and_close_out_fds(&mut self) {
        for slot in &mut self.out_fds[..self.out_fd_count] {
            *slot = None;
        }
        self.out_fd_count = 0;
    }
}

// anything larger than 4 KiB going into a pipe, we try to increase the pipe size for better performance.
const PIPE_DATA_GROW_THRESHOLD: u64 = 4096;

const PIPE_GROW_SIZE: i32 = 1 << 20;

pub fn serve_bytes(out: OwnedFd, payload: &[u8]) -> std::io::Result<()> {
    let (_, mut out) = prepare_out_fd(out, payload.len() as u64)?;

    out.write_all(payload)
}

/// Serves the payload to the given output file descriptor. If the output is a pipe and the payload is large, it may use a two-stage splice for better performance.
/// The payload must not change while it is served - splice hands the paster references to its cached pages.
pub fn serve_file(out: OwnedFd, payload: &File, payload_len: u64) -> std::io::Result<()> {
    let (is_pipe, mut out) = prepare_out_fd(out, payload_len)?;

    if is_pipe {
        log_debug!("Using two-stage splice for FIFO(pipe) output");
        return write_all_two_stage_splice(&out, payload, payload_len);
    }

    // TODO: Consider vec pool.
    const BUFFER_SIZE: usize = 256 * 1024;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut total_written = 0;
    while total_written < payload_len {
        let remaining = (payload_len - total_written).min(BUFFER_SIZE as u64) as usize;
        payload.read_exact_at(&mut buffer[..remaining], total_written)?;
        total_written += remaining as u64;
        out.write_all(&buffer[..remaining])?;
    }
    Ok(())
}

fn prepare_out_fd(out: OwnedFd, payload_len: u64) -> std::io::Result<(bool, File)> {
    // SAFETY: F_GETFL takes no argument, F_SETFL one int.
    let flags = unsafe { fcntl(out.as_raw_fd(), F_GETFL) };
    if flags != -1 && flags & O_NONBLOCK != 0 {
        // SAFETY: flags come from F_GETFL; only O_NONBLOCK is removed.
        // We do this to clear O_NONBLOCK if set to ensure blocking writes.
        let fcntl_result = unsafe { fcntl(out.as_raw_fd(), F_SETFL, flags & !O_NONBLOCK) };
        if fcntl_result == -1 {
            log_debug!(
                "Failed to clear O_NONBLOCK on output FD: {}",
                io::Error::last_os_error()
            );
        }
    }

    let out = File::from(out);
    let is_pipe = out.metadata()?.file_type().is_fifo();

    if is_pipe && payload_len > PIPE_DATA_GROW_THRESHOLD {
        // SAFETY: out is an open pipe; F_GETPIPE_SZ takes no argument, F_SETPIPE_SZ one int.
        if unsafe { fcntl(out.as_raw_fd(), F_GETPIPE_SZ) } < PIPE_GROW_SIZE {
            // SAFETY: same open pipe; F_SETPIPE_SZ takes one int.
            unsafe { fcntl(out.as_raw_fd(), F_SETPIPE_SZ, PIPE_GROW_SIZE) }; // errors ignored: only speed is lost
        }
    }
    Ok((is_pipe, out))
}

// Use a 2 stage splice to do zero(user space) copy from the payload file to the output pipe.
// Two stages overlap the page-cache lookup of our file with the paster draining its pipe: ~20% faster on tmpfs for 100 MiB.
// A direct splice keeps the paster's pipe locked during that lookup, so we and the paster take turns (~100 waits per 100 MiB assuming 1MB pipe).
fn write_all_two_stage_splice(out: &File, payload: &File, len: u64) -> io::Result<()> {
    let payload_fd = payload.as_raw_fd();
    let mut temp_in_offset = 0i64;
    let out_fd = out.as_raw_fd();

    let (reader, writer) = io::pipe()?;
    let temp_fd_writer = writer.as_raw_fd();
    let temp_fd_reader = reader.as_raw_fd();

    const TEMP_PIPE_GROW_SIZE: i32 = 1 << 20;
    // SAFETY: fd is an open pipe we own and F_SETPIPE_SZ is a valid fcntl command for setting the pipe size, which takes one arg.
    // If this fails we are OK to ignore and proceed.
    unsafe { fcntl(temp_fd_writer, F_SETPIPE_SZ, TEMP_PIPE_GROW_SIZE) };

    while temp_in_offset < len as i64 {
        // SAFETY: the payload fd and both pipe ends are open, and temp_in_offset is a valid i64
        let mut pending_bytes = unsafe {
            splice(
                payload_fd,
                &mut temp_in_offset,
                temp_fd_writer,
                ptr::null_mut(),
                TEMP_PIPE_GROW_SIZE as usize,
                0,
            )
        };

        if pending_bytes == 0 {
            // should not happen as being in the loop means there are more bytes to move.
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "Unexpected EOF of payload while splicing",
            ));
        }

        if pending_bytes == -1 {
            let err = io::Error::last_os_error();
            match err.kind() {
                ErrorKind::Interrupted => continue,
                _ => return Err(err),
            }
        }

        while pending_bytes > 0 {
            // SAFETY: both pipe fds are open for the whole call.
            let temp_written = unsafe {
                splice(
                    temp_fd_reader,
                    ptr::null_mut(),
                    out_fd,
                    ptr::null_mut(),
                    pending_bytes as usize,
                    0,
                )
            };

            if temp_written == -1 {
                let err = io::Error::last_os_error();
                match err.kind() {
                    ErrorKind::Interrupted => continue,
                    _ => return Err(err),
                }
            }

            if temp_written == 0 {
                return Err(io::Error::new(
                    ErrorKind::WriteZero,
                    "paste pipe accepted no data",
                ));
            }

            pending_bytes -= temp_written;
        }
    }

    debug_assert!(
        temp_in_offset == len as i64,
        "Not all input bytes were spliced"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        fs::OpenOptions,
        io::{Read, Seek},
        os::unix::{fs::OpenOptionsExt, net::UnixStream},
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::Duration,
    };

    // Larger than a grown 1 MiB pipe, so serving has to wait for the reader.
    const LEN: usize = 3 << 20;

    type Serve<'a> = Box<dyn Fn(OwnedFd) -> io::Result<()> + 'a>;

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
    fn a_non_blocking_paste_pipe_with_a_slow_reader_still_gets_everything() {
        let expected = data(LEN);
        for (what, serve) in servers(&expected) {
            let (mut reader, writer) = io::pipe().unwrap();
            // SAFETY: writer is an open pipe; F_SETFL takes one int.
            assert_eq!(unsafe { fcntl(writer.as_raw_fd(), F_SETFL, O_NONBLOCK) }, 0);
            let reading = thread::spawn(move || {
                // Let the pipe fill up first, so a non-blocking write would hit EAGAIN.
                thread::sleep(Duration::from_millis(50));
                let mut out = Vec::new();
                reader.read_to_end(&mut out).unwrap();
                out
            });
            serve(writer.into()).unwrap();
            assert_same(&reading.join().unwrap(), &expected, what);
        }
    }

    #[test]
    fn clearing_o_nonblock_keeps_the_pasters_other_flags() {
        let path = unique_temp_path();
        let out = OpenOptions::new()
            .append(true)
            .create_new(true)
            .custom_flags(O_NONBLOCK)
            .open(&path)
            .unwrap();
        fs::remove_file(&path).unwrap();
        // SAFETY: out is open; F_GETFL takes no argument.
        let before = unsafe { fcntl(out.as_raw_fd(), F_GETFL) };
        assert_ne!(before & O_NONBLOCK, 0);

        serve_bytes(out.try_clone().unwrap().into(), b"x").unwrap();

        // SAFETY: as above; the clone shares the open file description, so its flags.
        let after = unsafe { fcntl(out.as_raw_fd(), F_GETFL) };
        assert_eq!(after, before & !O_NONBLOCK);
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
            // SAFETY: writer is an open pipe; F_SETPIPE_SZ takes one int.
            unsafe { fcntl(writer.as_raw_fd(), F_SETPIPE_SZ, start) };
            let reading = thread::spawn(move || {
                let mut out = Vec::new();
                reader.read_to_end(&mut out).unwrap();
                out.len()
            });
            serve_bytes(writer.into(), &data(len)).unwrap();
            assert_eq!(reading.join().unwrap(), len);
            // SAFETY: probe is an open read end of the same pipe; F_GETPIPE_SZ takes no argument.
            let size = unsafe { fcntl(probe.as_raw_fd(), F_GETPIPE_SZ) };
            assert_eq!(size, expected, "len {len}, started at {start}");
        }
    }

    #[test]
    fn serving_leaves_no_file_descriptors_open() {
        let open_fds = || fs::read_dir("/proc/self/fd").unwrap().count();
        let expected = data(4097);
        let both = servers(&expected);
        let before = open_fds();
        for _ in 0..100 {
            for (_, serve) in &both {
                let (out, reading) = pipe_reader();
                serve(out).unwrap();
                reading.join().unwrap();
            }
        }
        // Other tests run in parallel and open a few fds; a leak would add at least 200.
        let after = open_fds();
        assert!(after < before + 50, "{before} fds before, {after} after");
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

    fn dev_null() -> OwnedFd {
        File::open("/dev/null").unwrap().into()
    }

    // Reads EOF only once every other handle to the peer socket is closed.
    fn assert_peer_closed(mut keep: UnixStream) {
        keep.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        assert_eq!(keep.read(&mut [0u8; 1]).unwrap(), 0);
    }

    #[test]
    fn in_fds_pop_in_fifo_order_across_wraparound() {
        let mut buf = InFdBuffer::new();
        // 3 does not divide FD_BUFFER_LEN, so some rounds straddle the wrap point.
        for _ in 0..FD_BUFFER_LEN {
            let fds: [OwnedFd; 3] = std::array::from_fn(|_| dev_null());
            let expected = fds.each_ref().map(|fd| fd.as_raw_fd());
            for fd in fds {
                buf.push_in_fd(fd).unwrap();
            }
            for raw in expected {
                assert_eq!(buf.pop_last_in_fd().unwrap().as_raw_fd(), raw);
            }
        }
        assert!(buf.pop_last_in_fd().is_none());
    }

    #[test]
    fn in_fds_overflow_is_an_error() {
        let mut buf = InFdBuffer::new();
        for _ in 0..FD_BUFFER_LEN {
            buf.push_in_fd(dev_null()).unwrap();
        }
        assert!(buf.push_in_fd(dev_null()).is_err());
    }

    #[test]
    fn queued_fds_are_closed_when_the_buffer_is_dropped() {
        let (keep_in, queued_in) = UnixStream::pair().unwrap();
        let (keep_out, queued_out) = UnixStream::pair().unwrap();
        let mut in_fds = InFdBuffer::new();
        let mut out_fds = OutFdBuffer::new();
        in_fds.push_in_fd(queued_in.into()).unwrap();
        out_fds.push_out_fd(queued_out.into()).unwrap();
        drop(in_fds);
        drop(out_fds);
        assert_peer_closed(keep_in);
        assert_peer_closed(keep_out);
    }
}
