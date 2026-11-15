use std::{
    io,
    os::fd::AsFd,
    path::PathBuf,
    thread::{self, JoinHandle},
    time::Duration,
};

use clipleaf_core::{
    ffi::poll::{PollEvents, PollFd, poll},
    log_error,
};
use clipleaf_ipc::{
    client::{Client, error::ClientError},
    protocol::{self, v1::models::ListReply},
};

use crate::pubsub::{self, Pub, Sub};

pub enum ClientCommand {
    SetVisible,
    SetHidden,
}

pub enum ClientEvent {
    Connected { connection_id: u64, daemon: Option<String> },
    Disconnected,
    List { connection_id: u64, list: ListReply },
}

pub struct DaemonClient {
    connection_id: u64,
    cmd_receiver: Sub<ClientCommand>,
    ev_sender: Pub<ClientEvent>,
    daemon_name: Option<String>,
    client: Option<Client>,
    socket_path: PathBuf,
    visible: bool,
}

impl DaemonClient {
    pub fn new(cmd_receiver: Sub<ClientCommand>) -> io::Result<(Self, Sub<ClientEvent>)> {
        let (ev_pub, ev_sub) = pubsub::channel(32)?;
        Ok((
            Self {
                connection_id: 0,
                cmd_receiver,
                ev_sender: ev_pub,
                daemon_name: None,
                client: None,
                visible: false,
                socket_path: clipleaf_ipc::default_socket_path()?,
            },
            ev_sub,
        ))
    }

    fn run(mut self) {
        loop {
            if let Err(e) = self.run_loop() {
                log_error!("DaemonClient encountered an error: {e}. Stopping worker thread.");
                break;
            }
        }
    }

    fn run_loop(&mut self) -> Result<(), ClientError> {
        loop {
            if self.client.is_none() {
                match Client::connect_handshake(&self.socket_path, 4096, 64 * 1024, Some(Duration::from_secs(1))) {
                    Ok(mut client) => {
                        self.connection_id = self.connection_id.wrapping_add(1);
                        self.daemon_name = client.daemon_name().map(|s| s.to_string());
                        self.ev_sender.publish(ClientEvent::Connected {
                            connection_id: self.connection_id,
                            daemon: self.daemon_name.clone(),
                        })?;
                        match if self.visible {
                            Self::on_visible(&mut client, self.connection_id, &self.ev_sender)
                        } else {
                            Ok(())
                        } {
                            Ok(()) => self.client = Some(client),
                            Err(e) => {
                                log_error!("List after connect failed: {e}");
                                self.ev_sender.publish(ClientEvent::Disconnected)?;
                            }
                        }
                    }
                    Err(e) => {
                        log_error!("Failed to connect to daemon: {e}");
                        if let Err(e) = self.ev_sender.publish(ClientEvent::Disconnected) {
                            log_error!("Failed to send Disconnected event: {e}");
                            return Err(e.into());
                        }
                    }
                }
            }

            let mut res = Ok(());
            if let Some(client) = self.client.as_mut() {
                res = Self::poll_client_and_cmds(client, &self.cmd_receiver, &mut self.visible, self.connection_id, &self.ev_sender);
            } else {
                let mut fds = [PollFd::new(self.cmd_receiver.fd(), PollEvents::IN)];
                let timeout = { if self.visible { Some(Duration::from_secs(1)) } else { None } };
                match poll(&mut fds, timeout) {
                    Ok(_) => {
                        if fds[0].revents().has_any(PollEvents::IN) {
                            res = Self::handle_client_commands(&mut None, &self.cmd_receiver, &mut self.visible, self.connection_id, &self.ev_sender);
                        }
                    }
                    Err(e) => {
                        log_error!("Error while polling for commands: {e}");
                        res = Err(ClientError::Io(e));
                    }
                }
            }

            if let Err(e) = res {
                log_error!("Error while reading event from daemon: {e}");
                self.client = None;
                if let Err(e) = self.ev_sender.publish(ClientEvent::Disconnected) {
                    log_error!("Failed to send Disconnected event: {e}");
                    return Err(e.into());
                }
            }
        }
    }

    fn on_visible(client: &mut Client, connection_id: u64, sender: &Pub<ClientEvent>) -> Result<(), ClientError> {
        let reply = client.call(&protocol::v1::models::List {
            query: None,
            offset: 0,
            limit: Some(100),
            thumbs: true,
        })?;

        sender
            .publish(ClientEvent::List { connection_id, list: reply })
            .map_err(ClientError::Io)?;

        Self::handle_client_events(client)?;

        Ok(())
    }

    fn poll_client_and_cmds(
        client: &mut Client,
        cmd_receiver: &Sub<ClientCommand>,
        visible: &mut bool,
        connection_id: u64,
        sender: &Pub<ClientEvent>,
    ) -> Result<(), ClientError> {
        let mut fds = [
            PollFd::new(cmd_receiver.fd(), PollEvents::IN),
            PollFd::new(client.as_fd(), PollEvents::IN),
        ];
        poll(&mut fds, None)?;
        let (cmd_receiver_evs, client_evs) = (fds[0].revents(), fds[1].revents());

        if client_evs.has_any(PollEvents::IN | PollEvents::ERR | PollEvents::HUP) {
            Self::handle_client_events(client)?;
        }

        if cmd_receiver_evs.has_any(PollEvents::IN) {
            return Self::handle_client_commands(&mut Some(client), cmd_receiver, visible, connection_id, sender);
        }

        Ok(())
    }

    fn handle_client_events(client: &mut Client) -> Result<(), ClientError> {
        loop {
            match client.next_event(Some(Duration::ZERO)) {
                Ok(Some(_event)) => {}
                Ok(None) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn handle_client_commands(
        client: &mut Option<&mut Client>,
        cmd_receiver: &Sub<ClientCommand>,
        visible: &mut bool,
        connection_id: u64,
        sender: &Pub<ClientEvent>,
    ) -> Result<(), ClientError> {
        let mut shown = false;
        cmd_receiver.recv_all(|c| match c {
            ClientCommand::SetVisible => {
                *visible = true;
                shown = true;
            }
            ClientCommand::SetHidden => *visible = false,
        });
        match client {
            Some(c) if shown && *visible => Self::on_visible(c, connection_id, sender),
            _ => Ok(()),
        }
    }
}

pub fn spawn_client_worker(client: DaemonClient) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("daemon-client".into()).spawn(move || {
        client.run();
    })
}
