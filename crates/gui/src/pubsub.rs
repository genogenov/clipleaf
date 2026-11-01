use std::{
    io,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, TrySendError},
    },
};

use clipleaf_core::ffi::event_fd::EventFd;
use gtk4::glib;

pub struct Pub<T> {
    tx: SyncSender<T>,
    efd: Arc<EventFd>,
}

impl<T> Clone for Pub<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            efd: self.efd.clone(),
        }
    }
}

pub struct Sub<T> {
    rx: Receiver<T>,
    efd: Arc<EventFd>,
}

pub fn channel<T>(cap: usize) -> io::Result<(Pub<T>, Sub<T>)> {
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    let efd = Arc::new(EventFd::new()?);
    Ok((Pub { tx, efd: efd.clone() }, Sub { rx, efd }))
}

impl<T> Pub<T> {
    pub fn publish(&self, msg: T) -> io::Result<()> {
        self.tx
            .send(msg)
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, format!("Pubsub channel closed: {}", e)))?;
        self.efd.signal()
    }

    pub fn try_publish(&self, msg: T) -> Result<(), TrySendError<T>> {
        self.tx.try_send(msg)?;
        let _ = self.efd.signal();
        Ok(())
    }
}

impl<T> Sub<T> {
    pub fn recv_all(&self, mut f: impl FnMut(T)) {
        let _ = self.efd.drain();
        while let Ok(msg) = self.rx.try_recv() {
            f(msg);
        }
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.efd.as_fd()
    }
}

impl<T: 'static> Sub<T> {
    pub fn subscribe(self, mut f: impl FnMut(T) + 'static) {
        let fd = self.efd.as_fd().as_raw_fd();
        glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
            self.recv_all(&mut f);
            glib::ControlFlow::Continue
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc::TrySendError, time::Duration};

    use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};

    use super::*;

    fn readable(fd: BorrowedFd<'_>) -> bool {
        let mut fds = [PollFd::new(fd, PollEvents::IN)];
        matches!(poll(&mut fds, Some(Duration::ZERO)).expect("poll"), PollResult::Ready(_))
    }

    /// Only after a publish: `recv_all` starts with a blocking eventfd read.
    fn drain(sub: &Sub<u32>) -> Vec<u32> {
        let mut got = Vec::new();
        sub.recv_all(|m| got.push(m));
        got
    }

    #[test]
    fn publish_then_recv_all_delivers_in_order_and_clears_the_fd() {
        let (tx, rx) = channel(8).unwrap();
        assert!(!readable(rx.fd()), "readable before any publish");

        tx.publish(1).unwrap();
        assert!(readable(rx.fd()), "publish must make the fd readable");
        assert_eq!(drain(&rx), [1]);
        assert!(!readable(rx.fd()), "still readable after recv_all");

        tx.try_publish(2).unwrap();
        assert!(readable(rx.fd()), "try_publish must make the fd readable");
        assert_eq!(drain(&rx), [2]);
        assert!(!readable(rx.fd()), "still readable after recv_all");

        tx.publish(3).unwrap();
        tx.clone().publish(4).unwrap();
        tx.try_publish(5).unwrap();
        assert_eq!(drain(&rx), [3, 4, 5], "one recv_all delivers everything, in order");
        assert!(!readable(rx.fd()), "still readable after recv_all");
    }

    #[test]
    fn try_publish_on_a_full_channel_is_full() {
        let (tx, rx) = channel(1).unwrap();
        tx.try_publish(1).unwrap();
        assert!(matches!(tx.try_publish(2), Err(TrySendError::Full(2))));
        assert_eq!(drain(&rx), [1]);
        tx.try_publish(3).unwrap();
        assert_eq!(drain(&rx), [3]);
    }
}
