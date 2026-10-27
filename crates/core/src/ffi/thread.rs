use std::{io, time::Duration};

use crate::ffi::definitions::{CLOCK_THREAD_CPUTIME_ID, PRIO_PROCESS, clock_gettime, getpriority, setpriority, timespec};

pub fn lower_current_thread_priority(by: u8) -> io::Result<i32> {
    // SAFETY: integer arguments only. PRIO_PROCESS with who 0 is the calling thread, which always exists.
    let now = unsafe { getpriority(PRIO_PROCESS, 0) };
    let target = (now + i32::from(by)).min(19);
    // SAFETY: integer arguments only; raising the nice value needs no privilege.
    if unsafe { setpriority(PRIO_PROCESS, 0, target) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(target)
}

pub fn thread_cpu_time() -> Duration {
    let mut timespec = timespec { tv_sec: 0, tv_nsec: 0 };

    // SAFETY: gettime is a valid Unix syscall,  CLOCK_THREAD_CPUTIME_ID is a valid clockid, and timespec is a valid pointer we control
    let res = unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut timespec as *mut timespec) };

    if res < 0 {
        return Duration::ZERO;
    }

    Duration::from_secs(timespec.tv_sec as u64).saturating_add(Duration::from_nanos(timespec.tv_nsec as u64))
}
