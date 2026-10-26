use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::time::Duration;
use std::{
    fs::File,
    io::ErrorKind,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::{FileExt, FileTypeExt},
    },
    ptr,
};

use crate::ffi::definitions::{F_GETPIPE_SZ, F_SETPIPE_SZ, fcntl, splice};
use crate::ffi::fd::set_nonblocking;
use crate::ffi::poll::{PollEvents, PollFd, PollResult, poll};
use crate::log_debug;

pub const IN_FD_BUFFER_LEN: usize = 64;

// kernel limit - everything above gets truncated.
pub const OUT_FD_BUFFER_LEN: usize = 28;

pub struct InFdBuffer {
    in_fds: [Option<OwnedFd>; IN_FD_BUFFER_LEN],
    in_fd_count: usize,
    in_fds_cursor: usize,
}

pub struct OutFdBuffer {
    out_fds: [Option<OwnedFd>; OUT_FD_BUFFER_LEN],
    out_fd_count: usize,
}

impl InFdBuffer {
    pub fn new() -> Self {
        Self {
            in_fds: [const { None }; IN_FD_BUFFER_LEN],
            in_fd_count: 0,
            in_fds_cursor: 0,
        }
    }

    pub fn pop_in_fd(&mut self) -> Option<OwnedFd> {
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
            out_fds: [const { None }; OUT_FD_BUFFER_LEN],
            out_fd_count: 0,
        }
    }

    pub fn is_full(&self) -> bool {
        self.out_fd_count >= self.out_fds.len()
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
    let (_, mut out) = prepare_out_fd(out, payload.len() as u64, false)?;

    out.write_all(payload)
}

pub fn serve_bytes_nonblocking(
    out: OwnedFd,
    payload: &[u8],
    stall_timeout: Duration,
) -> std::io::Result<()> {
    let (_, mut out) = prepare_out_fd(out, payload.len() as u64, true)?;
    write_all_nonblocking(&mut out, payload, stall_timeout)
}

/// Serves the payload to the given output file descriptor. If the output is a pipe and the payload is large, it may use a two-stage splice for better performance.
/// The payload must not change while it is served - splice hands the paster references to its cached pages.
pub fn serve_file(out: OwnedFd, payload: &File, payload_len: u64) -> std::io::Result<()> {
    let (is_pipe, mut out) = prepare_out_fd(out, payload_len, false)?;

    if is_pipe {
        log_debug!("Using two-stage splice for FIFO(pipe) output");
        return write_all_two_stage_splice(&out, payload, payload_len, None);
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

pub fn serve_file_nonblocking(
    out: OwnedFd,
    payload: &File,
    payload_len: u64,
    stall_timeout: Duration,
) -> std::io::Result<()> {
    let (is_pipe, mut out) = prepare_out_fd(out, payload_len, true)?;

    if is_pipe {
        log_debug!("Using two-stage splice for FIFO(pipe) output");
        return write_all_two_stage_splice(&out, payload, payload_len, Some(stall_timeout));
    }

    // TODO: Consider vec pool.
    const BUFFER_SIZE: usize = 256 * 1024;
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut total_written = 0;
    while total_written < payload_len {
        let remaining = (payload_len - total_written).min(BUFFER_SIZE as u64) as usize;
        payload.read_exact_at(&mut buffer[..remaining], total_written)?;
        total_written += remaining as u64;
        write_all_nonblocking(&mut out, &buffer[..remaining], stall_timeout)?;
    }
    Ok(())
}

fn write_all_nonblocking(
    out: &mut File,
    payload: &[u8],
    stall_timeout: Duration,
) -> io::Result<()> {
    let mut written = 0;
    while written < payload.len() {
        match out.write(&payload[written..]) {
            Ok(w) => {
                if w == 0 {
                    return Err(io::Error::new(
                        ErrorKind::WriteZero,
                        "pipe accepted no data",
                    ));
                }
                written += w;
            }
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock => {
                    wait_writable(out.as_fd(), stall_timeout)?;
                }
                ErrorKind::Interrupted => {
                    continue;
                }
                _ => return Err(e),
            },
        }
    }
    Ok(())
}

fn wait_writable(out: BorrowedFd, stall: Duration) -> io::Result<()> {
    let mut fds = [PollFd::new(out, PollEvents::OUT)];
    match poll(&mut fds, Some(stall))? {
        PollResult::Timeout => Err(io::Error::new(
            ErrorKind::TimedOut,
            "paster stopped reading",
        )),
        PollResult::Ready(_) => Ok(()),
    }
}

fn prepare_out_fd(
    out: OwnedFd,
    payload_len: u64,
    nonblocking: bool,
) -> std::io::Result<(bool, File)> {
    // We do this to set O_NONBLOCK to ensure (non)blocking writes.
    if let Err(e) = set_nonblocking(out.as_fd(), nonblocking) {
        // Treat only setting the "nonblocking" as hard err.
        if nonblocking {
            return Err(e);
        }
        log_debug!("Failed to clear O_NONBLOCK on output FD: {e}");
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
fn write_all_two_stage_splice(
    out: &File,
    payload: &File,
    len: u64,
    nonblock_stall_timeout: Option<Duration>,
) -> io::Result<()> {
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
        // Never past `len`: a payload file that grew after it was recorded must not leak its
        // extra bytes into the paste.
        let chunk = (len - temp_in_offset as u64).min(TEMP_PIPE_GROW_SIZE as u64) as usize;
        // SAFETY: the payload fd and both pipe ends are open, and temp_in_offset is a valid i64
        let mut pending_bytes = unsafe {
            splice(
                payload_fd,
                &mut temp_in_offset,
                temp_fd_writer,
                ptr::null_mut(),
                chunk,
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
                    ErrorKind::WouldBlock if let Some(stall) = nonblock_stall_timeout => {
                        wait_writable(out.as_fd(), stall)?;
                        continue;
                    }
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
    use std::{io::Read, os::unix::net::UnixStream};

    fn dev_null() -> OwnedFd {
        File::open("/dev/null").unwrap().into()
    }

    // The peer of a closed fd reads EOF at once. A read that would block means some copy of the
    // fd is still open. (Nothing in this test binary scans fds or spawns processes, so EOF is
    // never delayed by a transient reference.)
    fn assert_peer_closed(mut keep: UnixStream) {
        keep.set_nonblocking(true).unwrap();
        assert_eq!(keep.read(&mut [0u8; 1]).unwrap(), 0);
    }

    #[test]
    fn in_fds_pop_in_fifo_order_across_wraparound() {
        let mut buf = InFdBuffer::new();
        // 3 does not divide IN_FD_BUFFER_LEN, so some rounds straddle the wrap point.
        for _ in 0..IN_FD_BUFFER_LEN {
            let fds: [OwnedFd; 3] = std::array::from_fn(|_| dev_null());
            let expected = fds.each_ref().map(|fd| fd.as_raw_fd());
            for fd in fds {
                buf.push_in_fd(fd).unwrap();
            }
            for raw in expected {
                assert_eq!(buf.pop_in_fd().unwrap().as_raw_fd(), raw);
            }
        }
        assert!(buf.pop_in_fd().is_none());
    }

    #[test]
    fn in_fds_overflow_is_an_error() {
        let mut buf = InFdBuffer::new();
        for _ in 0..IN_FD_BUFFER_LEN {
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
