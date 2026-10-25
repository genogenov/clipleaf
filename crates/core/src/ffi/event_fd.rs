use std::{
    fs::File,
    io::{self, Error, Read, Write},
    os::fd::{AsFd, FromRawFd},
};

use crate::ffi::definitions::{EFD_CLOEXEC, eventfd};

pub struct EventFd(File);

impl EventFd {
    pub fn new() -> io::Result<Self> {
        // SAFETY: part of the standard C lib
        let efd = unsafe { eventfd(0, EFD_CLOEXEC) };

        if efd < 0 {
            return Err(Error::last_os_error());
        }

        // SAFETY: the eventfd was just created and we have checked for errors above.
        Ok(Self(unsafe { File::from_raw_fd(efd) }))
    }

    pub fn signal(&self) -> io::Result<()> {
        (&self.0).write_all(&1u64.to_ne_bytes())
    }

    pub fn drain(&self) -> io::Result<u64> {
        let mut bytes = [0u8; 8];
        (&self.0).read_exact(&mut bytes)?;
        Ok(u64::from_ne_bytes(bytes))
    }
}

impl AsFd for EventFd {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        self.0.as_fd()
    }
}
