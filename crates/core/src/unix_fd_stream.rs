use std::{
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    path::Path,
    ptr,
};

use crate::{
    fd_io::{InFdBuffer, OUT_FD_BUFFER_LEN},
    ffi::definitions::{
        CMSG_FD_OFFSET, MSG_CMSG_CLOEXEC, MSG_CTRUNC, MSG_DONTWAIT, MSG_NOSIGNAL, SCM_RIGHTS, SOL_SOCKET, cmsg_align, cmsg_space, cmsghdr, iovec,
        msghdr, recvmsg, sendmsg,
    },
};

const USIZE: usize = size_of::<usize>();

pub const CTRL_BUFFER_SIZE: usize = cmsg_space(OUT_FD_BUFFER_LEN * std::mem::size_of::<RawFd>());

#[repr(C)]
struct AlignedCmsghdr {
    _align: [cmsghdr; 0], // force cmsghdr alignment
    buff: [u8; CTRL_BUFFER_SIZE],
}

pub struct UnixFdStream {
    stream_fd: OwnedFd,
}

impl AsFd for UnixFdStream {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        self.stream_fd.as_fd()
    }
}

impl From<UnixStream> for UnixFdStream {
    fn from(stream: UnixStream) -> Self {
        Self { stream_fd: stream.into() }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum ReadMode {
    Block,

    NonBlock,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub(crate) enum WriteMode {
    Block,

    NonBlock,
}

impl UnixFdStream {
    pub fn connect(path: &Path) -> std::io::Result<Self> {
        Ok(UnixStream::connect(path)?.into())
    }

    pub(crate) fn read(&self, buffer: &mut [u8], fd_buffer: &mut InFdBuffer, mode: ReadMode) -> std::io::Result<usize> {
        let mut iovec = iovec {
            iov_base: buffer.as_mut_ptr(),
            iov_len: buffer.len(),
        };

        loop {
            let mut ctrl_buffer = AlignedCmsghdr {
                _align: [],
                buff: [0u8; CTRL_BUFFER_SIZE],
            };

            let mut msg = msghdr {
                msg_name: ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut iovec,
                msg_iovlen: 1,
                msg_control: ctrl_buffer.buff.as_mut_ptr() as *mut std::ffi::c_void,
                msg_controllen: ctrl_buffer.buff.len(),
                msg_flags: 0,
            };

            let mut flags = MSG_CMSG_CLOEXEC;
            if mode == ReadMode::NonBlock {
                flags |= MSG_DONTWAIT;
            }
            // SAFETY: msg points at iov/control buffers for the call; the kernel writes at most their lengths.
            let bytes_read_or_err = unsafe { recvmsg(self.stream_fd.as_raw_fd(), &mut msg, flags) };

            if bytes_read_or_err < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue; // retry on EINTR
                } else {
                    return Err(error);
                }
            }
            if bytes_read_or_err == 0 {
                return Ok(0); // EOF
            }

            let mut ctrl = &ctrl_buffer.buff[..msg.msg_controllen];
            while let Some(hdr) = ctrl.first_chunk::<CMSG_FD_OFFSET>() {
                let len = usize::from_ne_bytes(hdr[..USIZE].try_into().unwrap());
                let level = i32::from_ne_bytes(hdr[USIZE..USIZE + 4].try_into().unwrap());
                let kind = i32::from_ne_bytes(hdr[USIZE + 4..USIZE + 8].try_into().unwrap());
                let data = ctrl
                    .get(CMSG_FD_OFFSET..len)
                    .ok_or_else(|| std::io::Error::other("Malformed control message"))?;
                if level == SOL_SOCKET && kind == SCM_RIGHTS {
                    // keep wrapping all incoming FDs in OwnedFd to prevent leaks
                    // this is arguably an overkill given this will terminate the process, but adding it here for extra safety..
                    let mut pushed = Ok(());
                    for raw in data.as_chunks::<4>().0 {
                        // SAFETY: the kernel just installed this fd via SCM_RIGHTS; nothing else owns it.
                        let fd = unsafe { OwnedFd::from_raw_fd(i32::from_ne_bytes(*raw)) };
                        if pushed.is_ok() {
                            pushed = fd_buffer.push_in_fd(fd);
                        }
                    }

                    pushed?;
                }
                ctrl = ctrl.get(cmsg_align(len)..).unwrap_or(&[]);
            }

            // intentionally after handlihng the fds to avoid leaking them if the read fails
            if msg.msg_flags & MSG_CTRUNC != 0 {
                return Err(std::io::Error::other("Control message truncated"));
            }

            return Ok(bytes_read_or_err as usize);
        }
    }

    pub fn write(&self, data: &[u8], fds: &[Option<OwnedFd>], mode: WriteMode) -> std::io::Result<usize> {
        if fds.is_empty() {
            self.send_all(&mut [], data, mode)
        } else if fds.len() > OUT_FD_BUFFER_LEN {
            Err(std::io::Error::other("Too many file descriptors to send"))
        } else {
            let fds_bytes_len: usize = fds.len() * std::mem::size_of::<RawFd>();
            let cmsg_space = cmsg_space(fds_bytes_len);
            let mut ctrl_buffer: AlignedCmsghdr = AlignedCmsghdr {
                _align: [],
                buff: [0u8; CTRL_BUFFER_SIZE],
            };
            let mut_ctrl_buffer = &mut ctrl_buffer.buff;
            mut_ctrl_buffer[..USIZE].copy_from_slice(&(CMSG_FD_OFFSET + fds_bytes_len).to_ne_bytes());
            mut_ctrl_buffer[USIZE..USIZE + 4].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
            mut_ctrl_buffer[USIZE + 4..USIZE + 8].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
            for (dst, fd) in mut_ctrl_buffer[CMSG_FD_OFFSET..]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(fds.iter().flatten().map(AsRawFd::as_raw_fd))
            {
                dst.copy_from_slice(&fd.to_ne_bytes());
            }

            self.send_all(&mut mut_ctrl_buffer[..cmsg_space], data, mode)
        }
    }

    /// Returns the bytes sent - all of the data in Block mode.
    /// The fds were sent iff the result is > 0.
    fn send_all(&self, msg_control: &mut [u8], buff: &[u8], mode: WriteMode) -> std::io::Result<usize> {
        let mut total_bytes_sent = 0;
        let (mut control, mut ctrl_length) = match msg_control {
            [] => (ptr::null_mut(), 0),
            _ => (msg_control.as_mut_ptr() as *mut std::ffi::c_void, msg_control.len()),
        };

        // Do not kill the process on SIGPIPE - let it error out as EPIPE which we handle.
        let mut flags = MSG_NOSIGNAL;

        if mode == WriteMode::NonBlock {
            flags |= MSG_DONTWAIT;
        }

        loop {
            let mut iov = iovec {
                iov_base: buff[total_bytes_sent..].as_ptr() as *mut _,
                iov_len: buff.len() - total_bytes_sent,
            };

            let msg = msghdr {
                msg_name: ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut iov,
                msg_iovlen: 1,
                msg_control: control,
                msg_controllen: ctrl_length,
                msg_flags: 0,
            };

            // SAFETY: msg, iov and control point at buffers for the call; the kernel only reads them.
            let bytes_sent_or_err = unsafe { sendmsg(self.stream_fd.as_raw_fd(), &msg, flags) };

            if bytes_sent_or_err < 0 {
                let error = std::io::Error::last_os_error();

                match error.kind() {
                    std::io::ErrorKind::Interrupted => continue,
                    std::io::ErrorKind::WouldBlock if mode == WriteMode::NonBlock => {
                        return Ok(total_bytes_sent);
                    }
                    _ => return Err(error),
                }
            }

            // we need to adjust the iovec to point to the remaining data that needs to be sent
            total_bytes_sent += bytes_sent_or_err as usize;

            if total_bytes_sent < buff.len() {
                // we also need to detach the ctrl buffer to avoid resending the FDs - those went with the first partial send.
                control = ptr::null_mut();
                ctrl_length = 0;
                continue; // retry sending the remaining data
            }

            return Ok(total_bytes_sent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn dev_null() -> OwnedFd {
        File::open("/dev/null").unwrap().into()
    }

    #[test]
    fn write_rejects_more_fds_than_the_control_buffer_holds() {
        let (a, _b) = UnixStream::pair().unwrap();
        let stream = UnixFdStream::from(a);
        let fds: Vec<Option<OwnedFd>> = (0..=OUT_FD_BUFFER_LEN).map(|_| Some(dev_null())).collect();
        assert!(stream.write(b"x", &fds, WriteMode::Block).is_err());
    }
}
