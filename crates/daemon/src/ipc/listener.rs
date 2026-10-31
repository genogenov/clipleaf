use std::{
    io::{self, ErrorKind},
    os::{
        fd::AsFd,
        unix::net::{SocketAddr, UnixListener, UnixStream},
    },
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

use clipleaf_core::{
    ffi::{
        event_fd::EventFd,
        poll::{PollEvents, PollFd, poll},
    },
    log_debug, log_error, log_warn,
};

use crate::ipc::{client_connection::IpcClientConnection, message_handler::RequestMessageHandler};

const MAX_CONNECTIONS: usize = 32;

pub struct IpcListener<T>
where
    T: RequestMessageHandler,
{
    shutdown: Arc<EventFd>,
    listener: UnixListener,
    clients: Vec<JoinHandle<()>>,
    next_client_seq: u64,
    handler: T,
}

impl<T: RequestMessageHandler + 'static> IpcListener<T> {
    pub fn new(shutdown: Arc<EventFd>, listener: UnixListener, handler: T) -> Self {
        Self {
            shutdown,
            listener,
            next_client_seq: 0,
            clients: Vec::new(),
            handler,
        }
    }

    pub fn run(&mut self) {
        log_debug!("Hello from IPC listener");
        loop {
            let mut fds = [
                PollFd::new(self.listener.as_fd(), PollEvents::IN),
                PollFd::new(self.shutdown.as_fd(), PollEvents::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {
                    let (conn_ev, stop_ev) = (fds[0].revents(), fds[1].revents());
                    if stop_ev.has_any(PollEvents::IN) {
                        break;
                    }
                    if conn_ev.has_any(PollEvents::IN) {
                        loop {
                            match self.listener.accept() {
                                Ok((stream, addr)) => {
                                    if let Err(e) = self.accept_new(stream, addr) {
                                        log_error!("Failed accepting new connection: {e}");
                                    }
                                }
                                Err(e) => match e.kind() {
                                    ErrorKind::WouldBlock => break,
                                    ErrorKind::Interrupted | ErrorKind::ConnectionAborted => continue,
                                    _ => {
                                        log_error!("IPC listener: accept failed with OS error: {e}; backing off for 100 ms");
                                        let mut fds = [PollFd::new(self.shutdown.as_fd(), PollEvents::IN)];
                                        let _ = poll(&mut fds, Some(Duration::from_millis(100)));
                                        break;
                                    }
                                },
                            }
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    log_error!("IPC listener: poll failed: {e}; retrying in 100 ms");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }

        log_debug!("IPC listener shutting down");
        for client in self.clients.drain(..) {
            let name = client.thread().name().map(|x| x.to_string());
            if let Err(e) = client.join() {
                log_warn!("Client thread {name:?} panicked: {e:?}");
            } else {
                log_debug!("Client thread {name:?} finished.")
            }
        }
    }

    fn accept_new(&mut self, stream: UnixStream, addr: SocketAddr) -> io::Result<()> {
        log_debug!("Accepting new connection from {:?}", addr);

        while let Some(idx) = self.clients.iter().position(|t| t.is_finished()) {
            let t = self.clients.swap_remove(idx);
            let name = t.thread().name().map(|x| x.to_string());
            if let Err(e) = t.join() {
                log_warn!("Client thread {name:?} panicked: {e:?}");
            } else {
                log_debug!("Client thread {name:?} finished.")
            }
        }

        if self.clients.len() >= MAX_CONNECTIONS {
            log_warn!("IPC Listener: At connection limit - dropping new connection");
        } else {
            stream.set_nonblocking(true)?;
            let off_signal = self.shutdown.clone();
            let handler = self.handler.clone();
            let mut conn = IpcClientConnection::new(stream, off_signal, handler)?;
            let handle = thread::Builder::new().name(format!("ipc-conn-{}", self.next_client_seq)).spawn(move || {
                conn.run();
            })?;
            self.next_client_seq = self.next_client_seq.wrapping_add(1);
            self.clients.push(handle);
        }

        Ok(())
    }
}
