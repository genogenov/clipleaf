//! `poll` and `EventFd` through the public API, with plain pipes. No timing: a timeout is only
//! ever checked with nothing ready, so it can't fire early or late in a way the test sees.

use std::{
    io::{self, Write},
    os::fd::AsFd,
    time::Duration,
};

use clipleaf_core::ffi::{
    event_fd::EventFd,
    poll::{PollEvents, PollFd, PollResult, poll},
};

#[test]
fn only_the_ready_fd_is_reported() {
    let (idle_rx, _idle_tx) = io::pipe().unwrap();
    let (ready_rx, mut ready_tx) = io::pipe().unwrap();
    ready_tx.write_all(b"x").unwrap();

    let mut fds = [
        PollFd::new(idle_rx.as_fd(), PollEvents::IN),
        PollFd::new(ready_rx.as_fd(), PollEvents::IN),
    ];
    let result = poll(&mut fds, None).unwrap();

    assert!(matches!(result, PollResult::Ready(1)));
    assert_eq!(fds[0].revents(), PollEvents::EMPTY);
    assert_eq!(fds[1].revents(), PollEvents::IN);
}

#[test]
fn a_closed_write_end_is_reported_as_hang_up_without_being_asked_for() {
    let (rx, tx) = io::pipe().unwrap();
    drop(tx);
    let mut fds = [PollFd::new(rx.as_fd(), PollEvents::IN)];
    assert!(matches!(poll(&mut fds, None).unwrap(), PollResult::Ready(1)));
    assert!(fds[0].revents().has_any(PollEvents::HUP));
}

#[test]
fn nothing_ready_is_a_timeout_for_zero_and_for_a_fraction_of_a_millisecond() {
    let (rx, _tx) = io::pipe().unwrap();
    for timeout in [Duration::ZERO, Duration::from_micros(10)] {
        let mut fds = [PollFd::new(rx.as_fd(), PollEvents::IN)];
        assert!(matches!(poll(&mut fds, Some(timeout)).unwrap(), PollResult::Timeout), "{timeout:?}");
        assert_eq!(fds[0].revents(), PollEvents::EMPTY);
    }
}

#[test]
fn a_timeout_too_large_for_poll_waits_like_none() {
    // Instant + Duration::MAX overflows: no deadline, and the fd being ready ends the wait.
    let (rx, mut tx) = io::pipe().unwrap();
    tx.write_all(b"x").unwrap();
    let mut fds = [PollFd::new(rx.as_fd(), PollEvents::IN)];
    assert!(matches!(poll(&mut fds, Some(Duration::MAX)).unwrap(), PollResult::Ready(1)));
}

fn readable(event: &EventFd) -> bool {
    let mut fds = [PollFd::new(event.as_fd(), PollEvents::IN)];
    matches!(poll(&mut fds, Some(Duration::ZERO)).unwrap(), PollResult::Ready(_))
}

#[test]
fn an_event_fd_is_readable_from_the_first_signal_until_it_is_drained() {
    let event = EventFd::new().unwrap();
    assert!(!readable(&event));

    for _ in 0..3 {
        event.signal().unwrap();
    }
    assert!(readable(&event));
    // One drain returns every signal since the last one and makes it quiet again.
    assert_eq!(event.drain().unwrap(), 3);
    assert!(!readable(&event));

    event.signal().unwrap();
    assert_eq!(event.drain().unwrap(), 1);
}

#[test]
fn an_event_fd_is_close_on_exec() {
    // Set at creation (EFD_CLOEXEC), so a fork + exec elsewhere in the process can't inherit it.
    // Read back with F_GETFD (rustix, independent of core's per-arch constants).
    use clipleaf_test_common::rustix::io::{FdFlags, fcntl_getfd};
    let event = EventFd::new().unwrap();
    let flags = fcntl_getfd(event.as_fd()).unwrap();
    assert!(flags.contains(FdFlags::CLOEXEC), "{flags:?}");
}
