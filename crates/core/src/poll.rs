use std::{
    io::{self, Error, ErrorKind},
    marker::PhantomData,
    os::fd::{AsRawFd, BorrowedFd},
    time::{Duration, Instant},
};

use crate::ffi::{self, pollfd};

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PollFd<'fd> {
    raw: pollfd,
    _fd: PhantomData<BorrowedFd<'fd>>,
}
impl<'fd> PollFd<'fd> {
    pub fn new(fd: BorrowedFd<'fd>, events: i16) -> Self {
        PollFd {
            raw: pollfd {
                fd: fd.as_raw_fd(),
                events,
                revents: 0,
            },
            _fd: PhantomData,
        }
    }
    pub fn revents(&self) -> i16 {
        self.raw.revents
    }
}

pub enum PollResult {
    Timeout,

    Ready(usize),
}

/// Waits until one of `fds` is ready or `duration` passes (None: forever). Retries EINTR:
pub fn poll(fds: &mut [PollFd<'_>], duration: Option<Duration>) -> io::Result<PollResult> {
    let fds_ptr = fds.as_mut_ptr() as *mut pollfd;

    let deadline = duration.and_then(|t| Instant::now().checked_add(t));
    loop {
        let timeout_ms = match deadline {
            None => -1,
            // Round up: any time left is at least 1 ms, so a fraction never becomes a busy loop.
            Some(d) => d
                .saturating_duration_since(Instant::now())
                .as_nanos()
                .div_ceil(1_000_000)
                .min(i32::MAX as u128) as i32,
        };

        // SAFETY: PollFd is repr(transparent) over pollfd, so `fds` is a valid array of fds.len()
        // pollfds; each fd is borrowed (BorrowedFd<'fd>) and stays open for the call; the kernel
        // reads fd/events and writes only revents within the slice.
        let result = unsafe { ffi::poll(fds_ptr, fds.len() as std::ffi::c_ulong, timeout_ms) };
        if result == 0 {
            return Ok(PollResult::Timeout);
        }

        if result < 0 {
            let error = Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error);
            }
            continue;
        }

        return Ok(PollResult::Ready(result as usize));
    }
}
