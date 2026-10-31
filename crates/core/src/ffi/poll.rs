use std::{
    io::{self, Error, ErrorKind},
    marker::PhantomData,
    ops::{BitOr, BitOrAssign},
    os::fd::{AsRawFd, BorrowedFd},
    time::{Duration, Instant},
};

use crate::ffi::definitions::{self, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT, pollfd};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PollEvents(i16);
impl PollEvents {
    pub const EMPTY: Self = Self(0);
    pub const IN: Self = Self(POLLIN);
    pub const OUT: Self = Self(POLLOUT);
    pub const ERR: Self = Self(POLLERR);
    pub const HUP: Self = Self(POLLHUP);
    pub const NVAL: Self = Self(POLLNVAL);

    pub const fn has_any(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}
impl BitOr for PollEvents {
    type Output = Self;
    fn bitor(self, r: Self) -> Self {
        Self(self.0 | r.0)
    }
}

impl BitOrAssign for PollEvents {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PollFd<'fd> {
    raw: pollfd,
    _fd: PhantomData<BorrowedFd<'fd>>,
}
impl<'fd> PollFd<'fd> {
    pub fn new(fd: BorrowedFd<'fd>, events: PollEvents) -> Self {
        PollFd {
            raw: pollfd {
                fd: fd.as_raw_fd(),
                events: events.0,
                revents: 0,
            },
            _fd: PhantomData,
        }
    }
    pub fn revents(&self) -> PollEvents {
        PollEvents(self.raw.revents)
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
        let result = unsafe { definitions::poll(fds_ptr, fds.len() as std::ffi::c_ulong, timeout_ms) };
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
