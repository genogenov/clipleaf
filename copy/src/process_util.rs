use std::{env, fs, io, os::fd::AsRawFd};

use clipleaf_core::ffi::{SIG_ERR, SIG_IGN, SIGHUP, dup2, fork, setsid, signal};

pub enum Forked {
    Parent,
    Child,
}

pub fn fork_process() -> io::Result<Forked> {
    // The child stays in the terminal's foreground group until setsid; our exit as session leader would SIGHUP it.
    // SAFETY: installs SIG_IGN, no handler code; only changes this process's SIGHUP disposition.
    if unsafe { signal(SIGHUP, SIG_IGN) } == SIG_ERR {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: Forking is safe in our case because the parent process will exit immediately after forking, and there is only a single thread up to this point.
    let pid = unsafe { fork() };
    if pid < 0 {
        Err(io::Error::last_os_error())
    } else if pid == 0 {
        Ok(Forked::Child)
    } else {
        Ok(Forked::Parent)
    }
}

pub fn detach() -> io::Result<()> {
    // SAFETY: Takes no arguments and fails only if we are already a process group leader, which a fresh child isnt.
    if unsafe { setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }

    // Change the current working directory to the root to avoid holding a ref to any unmountable fs.
    env::set_current_dir("/")?;

    let dev_null = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;

    let dev_null_fd = dev_null.as_raw_fd();

    // SAFETY: We are duplicating the file descriptor for /dev/null to standard input and output.
    unsafe {
        let stdin_fd = io::stdin().as_raw_fd();
        if stdin_fd != dup2(dev_null_fd, stdin_fd) {
            return Err(io::Error::last_os_error());
        }
        let stdout_fd = io::stdout().as_raw_fd();
        if stdout_fd != dup2(dev_null_fd, stdout_fd) {
            return Err(io::Error::last_os_error());
        }

        // Revisit this - we might want to keep stderr if its a terminal.
        let stderr_fd = io::stderr().as_raw_fd();
        if stderr_fd != dup2(dev_null_fd, stderr_fd) {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
