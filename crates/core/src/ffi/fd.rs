use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd},
};

use crate::ffi::definitions::{F_GETFL, F_SETFL, F_SETPIPE_SZ, O_NONBLOCK, fcntl};

pub fn resize_pipe(fd: BorrowedFd, size: i32) -> io::Result<usize> {
    // SAFETY: F_SETPIPE_SZ is a valid fcntl command for setting the pipe size, which takes one arg.
    let res = unsafe { fcntl(fd.as_raw_fd(), F_SETPIPE_SZ, size) };

    if res > -1 {
        return Ok(res as usize);
    }

    Err(io::Error::last_os_error())
}

/// Sets or clears O_NONBLOCK on the open file description behind `fd` (shared with its dups).
pub fn set_nonblocking(fd: BorrowedFd, nonblock: bool) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: F_GETFL takes no argument and is a valid fcntl
    let flags = unsafe { fcntl(raw, F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }

    if has_nonblock(flags) != nonblock {
        // SAFETY: flags come from F_GETFL; only O_NONBLOCK is modified.
        let fcntl_result = unsafe { fcntl(raw, F_SETFL, set_nonblock(flags, nonblock)) };
        if fcntl_result == -1 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

fn set_nonblock(flags: i32, nonblock: bool) -> i32 {
    if nonblock { flags | O_NONBLOCK } else { flags & !O_NONBLOCK }
}

fn has_nonblock(flags: i32) -> bool {
    flags & O_NONBLOCK != 0
}
