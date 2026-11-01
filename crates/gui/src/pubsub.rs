use std::{
    io,
    os::fd::{AsFd, AsRawFd},
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

#[allow(dead_code)]
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
