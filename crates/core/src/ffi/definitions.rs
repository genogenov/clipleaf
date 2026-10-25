use std::{ffi::c_long, os::fd::RawFd};

unsafe extern "C" {
    /// Forks the current process, returning -1 on error, 0 in the child, and the child's PID in the parent. Link to doc: https://man7.org/linux/man-pages/man2/fork.2.html
    pub fn fork() -> i32;

    /// Creates a new session and sets the process group ID. Link to doc: https://man7.org/linux/man-pages/man2/setsid.2.html
    pub fn setsid() -> i32;

    /// Duplicates a file descriptor to a given new file descriptor number. Link to doc: https://man7.org/linux/man-pages/man2/dup2.2.html
    pub fn dup2(old: i32, new: i32) -> i32;

    /// Sets a signal handler for the given signal number. Link to doc: https://man7.org/linux/man-pages/man2/signal.2.html
    pub fn signal(signum: i32, handler: usize) -> usize;

    /// Receives a message from a socket, potentially including ancillary data (control messages). Link to doc: https://man7.org/linux/man-pages/man2/recvmsg.2.html
    pub fn recvmsg(sockfd: RawFd, msg: *mut msghdr, flags: i32) -> isize;

    /// Sends a message on a socket, potentially including ancillary data (control messages). Link to doc: https://man7.org/linux/man-pages/man2/sendmsg.2.html
    pub fn sendmsg(sockfd: RawFd, msg: *const msghdr, flags: i32) -> isize;

    /// Performs operations on a file descriptor. Link to doc: https://man7.org/linux/man-pages/man2/fcntl.2.html
    pub fn fcntl(fd: i32, cmd: i32, ...) -> i32;

    /// Moves data between two file descriptors without copying between kernel and user space. Link to doc: https://man7.org/linux/man-pages/man2/splice.2.html
    pub fn splice(
        fd_in: RawFd,
        off_in: *mut i64,
        fd_out: RawFd,
        off_out: *mut i64,
        len: usize,
        flags: u32,
    ) -> isize;

    /// Waits for events on a set of file descriptors. Link to doc: https://man7.org/linux/man-pages/man2/poll.2.html
    pub fn poll(fds: *mut pollfd, nfds: std::ffi::c_ulong, timeout: i32) -> i32;

    /// Creates a file descriptor for event notification. Link to doc: https://man7.org/linux/man-pages/man2/eventfd.2.html
    pub fn eventfd(initval: u32, flags: i32) -> i32;

    /// Sets the priority of a process, process group, or thread. Link to doc: https://man7.org/linux/man-pages/man2/setpriority.2.html
    pub fn setpriority(which: i32, who: u32, prio: i32) -> i32;

    /// Gets the priority of a process, process group, or thread. Link to doc: https://man7.org/linux/man-pages/man2/getpriority.2.html
    pub fn getpriority(which: i32, who: u32) -> i32;

    /// Retrieves the time of a specified clock. Link to doc: https://man7.org/linux/man-pages/man2/clock_gettime.2.html
    pub fn clock_gettime(clockid: i32, tp: *mut timespec) -> i32;
}

pub const F_GETPIPE_SZ: i32 = 1032;

pub const F_SETPIPE_SZ: i32 = 1031;

pub const F_GETFL: i32 = 3;

pub const F_SETFL: i32 = 4;

pub const EFD_CLOEXEC: i32 = O_CLOEXEC;

pub const SCM_RIGHTS: i32 = 0x01;

pub const MSG_CTRUNC: i32 = 0x8;

pub const MSG_CMSG_CLOEXEC: i32 = 0x40000000;

pub const MSG_DONTWAIT: i32 = 0x40;

pub const MSG_NOSIGNAL: i32 = 0x4000;

pub const CMSG_FD_OFFSET: usize = cmsg_align(std::mem::size_of::<cmsghdr>());

pub const SIGHUP: i32 = 1; // same on every Linux arch

pub const SIG_IGN: usize = 1;

pub const SIG_ERR: usize = usize::MAX;

// O_TMPFILE = __O_TMPFILE | O_DIRECTORY; differ between architectures.
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "m68k"
))]
pub const O_TMPFILE: i32 = (1 << 22) | (1 << 14);

#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
pub const O_TMPFILE: i32 = (1 << 25) | (1 << 16);
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "m68k",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
pub const O_TMPFILE: i32 = (1 << 22) | (1 << 16); // kernel default: x86, riscv, loongarch, s390x, mips

#[cfg(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
))]
pub const O_NONBLOCK: i32 = 1 << 7;

#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
pub const O_NONBLOCK: i32 = 1 << 14;

#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
pub const O_NONBLOCK: i32 = 1 << 11;

#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
pub const O_CLOEXEC: i32 = 1 << 22;

#[cfg(not(any(target_arch = "sparc", target_arch = "sparc64")))]
pub const O_CLOEXEC: i32 = 1 << 19;

#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
pub const SOL_SOCKET: i32 = 1;

pub const POLLIN: i16 = 0x1;
pub const POLLOUT: i16 = 0x4;
pub const POLLERR: i16 = 0x8;

pub const POLLHUP: i16 = 0x10;
pub const POLLNVAL: i16 = 0x20;

#[cfg(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "sparc",
    target_arch = "sparc64"
))]
pub const SOL_SOCKET: i32 = 0xffff;

pub const PRIO_PROCESS: i32 = 0;

pub const CLOCK_THREAD_CPUTIME_ID: i32 = 3;

#[repr(C)]
pub struct iovec {
    pub iov_base: *mut u8,
    pub iov_len: usize,
}

#[repr(C)]
pub struct msghdr {
    pub msg_name: *mut std::ffi::c_void,
    pub msg_namelen: u32,
    pub msg_iov: *mut iovec,
    pub msg_iovlen: usize,
    pub msg_control: *mut std::ffi::c_void,
    pub msg_controllen: usize,
    pub msg_flags: i32,
}

#[repr(C)]
pub struct cmsghdr {
    pub cmsg_len: usize,
    pub cmsg_level: i32,
    pub cmsg_type: i32,
    // followed by u8[] data
}

pub const fn cmsg_align(len: usize) -> usize {
    let align_to = std::mem::size_of::<usize>();
    (len + align_to - 1) & !(align_to - 1)
}

pub const fn cmsg_space(len: usize) -> usize {
    cmsg_align(std::mem::size_of::<cmsghdr>()) + cmsg_align(len)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct pollfd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

#[repr(C)]
pub struct timespec {
    pub tv_sec: c_long,
    pub tv_nsec: c_long,
}
