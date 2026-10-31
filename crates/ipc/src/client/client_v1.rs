use std::{
    collections::VecDeque,
    io::{self, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    time::{Duration, Instant},
};

use clipleaf_core::{
    ffi::poll::{PollEvents, PollFd, PollResult, poll},
    log_debug,
};

use crate::{
    client::error::ClientError,
    frame::frame_reader::FrameReader,
    protocol::{
        Message, Request,
        codec::{decode_body, encode_frame},
        handshake::{ErrorReply, Hello},
        header::{EventType, Header, MAX_FRAME_LEN, MAX_REQUEST_LEN, MessageType},
        v1::models::{Added, Removed, SelectionChanged, Updated},
    },
};

const SUPPORTED_VERSION: u32 = 1;
const EVENT_QUEUE_MAX_LEN: usize = 1024;

enum Incoming<'a> {
    Frame(Header, &'a [u8]),
    Consumed,
}

#[derive(Debug)]
pub enum Event {
    Added(Added),
    Updated(Updated),
    Removed(Removed),
    Cleared,
    SelectionChanged(SelectionChanged),
}

pub struct Client {
    stream: UnixStream,
    reader: FrameReader,
    serialize_buf: Vec<u8>,
    next_req_id: u32,
    events: VecDeque<(EventType, Vec<u8>)>,
    conn_version: u32,
    conn_features: Vec<String>,
    conn_daemon: Option<String>,
    poisoned: bool,
    event_queue_full: bool,
}

impl Client {
    pub fn connect_handshake(
        path: &Path,
        default_write_buffer_size: usize,
        default_read_buffer_size: usize,
        read_write_timeout: Option<Duration>,
    ) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)?;

        let reader = FrameReader::new(default_read_buffer_size, MAX_FRAME_LEN as usize);
        let serialize_buf = Vec::with_capacity(default_write_buffer_size);

        let mut client = Self {
            stream,
            reader,
            serialize_buf,
            events: VecDeque::new(),
            next_req_id: 1,
            conn_daemon: None,
            conn_features: Vec::new(),
            conn_version: 0,
            poisoned: false,
            event_queue_full: false,
        };

        client.set_call_timeout(read_write_timeout)?;

        let hello = Hello {
            features: Vec::new(),
            versions: vec![SUPPORTED_VERSION],
        };

        let response = client.call(&hello)?;
        log_debug!("Got Hello reply: {response:?}");

        if response.version != SUPPORTED_VERSION {
            return Err(ClientError::UnsupportedVersion);
        }

        client.conn_features = response.features;
        client.conn_version = response.version;
        client.conn_daemon = response.daemon;

        Ok(client)
    }

    pub fn set_call_timeout(&mut self, duration: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(duration)?;
        self.stream.set_write_timeout(duration)
    }

    pub fn call<R: Request>(&mut self, req: &R) -> Result<R::Reply, ClientError> {
        if self.poisoned {
            return Err(ClientError::Poisoned);
        }
        let req_id = self.next_req_id;

        self.serialize_buf.clear();
        encode_frame(req_id, req, &mut self.serialize_buf, MAX_REQUEST_LEN)?;
        log_debug!("Client: sending msg id {req_id} - {:?}({})", R::TYPE, self.serialize_buf.len());
        self.next_req_id = self.next_req_id.wrapping_add(1).max(1);
        if let Err(e) = self.stream.write_all(&self.serialize_buf) {
            self.poisoned = true;
            return Err(ClientError::Io(e));
        }

        loop {
            match self.handle_next_frame(Some(req_id)) {
                Some(incoming) => match incoming {
                    Ok(Incoming::Frame(header, body)) => {
                        log_debug!("Received reply for request ID {req_id} of type {:?}", header.msg_type);
                        match header.msg_type {
                            MessageType::Reply(_) if header.msg_type == R::Reply::TYPE => return Ok(decode_body::<R::Reply>(body)?),
                            MessageType::Error => {
                                let err = decode_body::<ErrorReply>(body)?;
                                return Err(ClientError::ResponseError(err));
                            }
                            _ => return Err(ClientError::UnexpectedType(header.msg_type)),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => return Err(e),
                },
                None => {
                    self.fill_buffer()?;
                }
            }
        }
    }

    pub fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>, ClientError> {
        let start = Instant::now();
        #[inline(always)]
        fn construct_event(event_type: EventType, body: &[u8]) -> Result<Option<Event>, ClientError> {
            match event_type {
                EventType::Added => Ok(Some(Event::Added(decode_body(body)?))),
                EventType::Updated => Ok(Some(Event::Updated(decode_body(body)?))),
                EventType::Removed => Ok(Some(Event::Removed(decode_body(body)?))),
                EventType::Cleared => Ok(Some(Event::Cleared)),
                EventType::SelectionChanged => Ok(Some(Event::SelectionChanged(decode_body(body)?))),
            }
        }

        loop {
            if std::mem::take(&mut self.event_queue_full) {
                return Err(ClientError::EventQueueFull);
            }

            if let Some(ev) = self.events.pop_front() {
                return construct_event(ev.0, ev.1.as_slice());
            }

            if self.poisoned {
                return Err(ClientError::Poisoned);
            }

            match self.handle_next_frame(None) {
                Some(incoming) => match incoming {
                    Ok(_) => {}
                    Err(e) => return Err(e),
                },
                None => {
                    let mut fds = [PollFd::new(self.stream.as_fd(), PollEvents::IN)];
                    let remaining = timeout.map(|t| t.saturating_sub(Instant::now().saturating_duration_since(start)));
                    match poll(&mut fds, remaining)? {
                        PollResult::Timeout => return Ok(None),
                        PollResult::Ready(_) => {}
                    }

                    self.fill_buffer()?;
                }
            }
        }
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn handle_next_frame<'a>(&'a mut self, req_id: Option<u32>) -> Option<Result<Incoming<'a>, ClientError>> {
        if let Some(res) = self.reader.next_frame() {
            match res {
                Err(e) => {
                    self.poisoned = true;
                    log_debug!("Got error response from FrameReader - marking client as poisoned.");
                    return Some(Err(ClientError::Protocol(e)));
                }
                Ok((header, body)) => match header.msg_type {
                    MessageType::Error if header.req_id == 0 => {
                        let err = match decode_body::<ErrorReply>(body) {
                            Ok(payload) => payload,
                            Err(e) => return Some(Err(ClientError::Protocol(e))),
                        };
                        self.poisoned = true;
                        return Some(Err(ClientError::ConnectionError(err)));
                    }
                    MessageType::Event(event_type) => {
                        log_debug!("Client: got event type {:?}({})", event_type, body.len());
                        if self.event_queue_full {
                            // marked full = skip
                        } else if self.events.len() >= EVENT_QUEUE_MAX_LEN {
                            self.event_queue_full = true;
                            self.events.clear();
                        } else {
                            self.events.push_back((event_type, body.into()));
                        }
                        return Some(Ok(Incoming::Consumed));
                    }
                    _ if req_id == Some(header.req_id) => return Some(Ok(Incoming::Frame(header, body))),
                    _ => {
                        log_debug!("Discarding frame with ID {} and type {:?}", header.req_id, header.msg_type);
                        return Some(Ok(Incoming::Consumed));
                    }
                },
            }
        }

        None
    }

    fn fill_buffer(&mut self) -> Result<usize, ClientError> {
        loop {
            match self.reader.fill_from(&mut self.stream) {
                Ok(0) => {
                    self.poisoned = true;
                    return Err(ClientError::EofClosed);
                }
                Ok(n) => {
                    return Ok(n);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Err(ClientError::Timeout);
                }
                Err(e) => {
                    self.poisoned = true;
                    return Err(ClientError::Io(e));
                }
            }
        }
    }
}
