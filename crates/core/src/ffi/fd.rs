use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd},
};

use crate::ffi::definitions::{F_SETPIPE_SZ, fcntl};

pub fn resize_pipe(fd: BorrowedFd, size: i32) -> io::Result<usize> {
    // SAFETY: F_SETPIPE_SZ is a valid fcntl command for setting the pipe size, which takes one arg.
    let res = unsafe { fcntl(fd.as_raw_fd(), F_SETPIPE_SZ, size) };

    if res > -1 {
        return Ok(res as usize);
    }

    Err(io::Error::last_os_error())
}
