use std::{
    collections::VecDeque,
    io::{self, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use clipleaf_core::log_debug;

use crate::{
    client::error::ClientError,
    frame::frame_reader::FrameReader,
    protocol::{
        Message, Request,
        codec::{decode_body, encode_frame},
        handshake::{ErrorReply, Hello},
        header::{EventType, MAX_FRAME_LEN, MAX_REQUEST_LEN, MessageType},
    },
};

const SUPPORTED_VERSION: u32 = 1;

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
        };

        client.set_timeout(read_write_timeout)?;

        let hello = Hello {
            features: Vec::new(),
            versions: vec![SUPPORTED_VERSION],
        };

        let response = client.call(&hello)?;

        if response.version != SUPPORTED_VERSION {
            return Err(ClientError::UnsupportedVersion);
        }

        client.conn_features = response.features;
        client.conn_version = response.version;
        client.conn_daemon = response.daemon;

        Ok(client)
    }

    pub fn set_timeout(&mut self, duration: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(duration)?;
        self.stream.set_write_timeout(duration)
    }

    pub fn call<R: Request>(&mut self, req: &R) -> Result<R::Reply, ClientError> {
        if self.poisoned {
            return Err(ClientError::Poisoned);
        }
        let req_id = self.next_req_id;

        self.serialize_buf.clear();
        encode_frame(req_id, req, &mut self.serialize_buf, MAX_REQUEST_LEN as usize)?;
        log_debug!("Client: sending msg id {req_id} - {:?}({})", R::TYPE, self.serialize_buf.len());
        self.next_req_id = self.next_req_id.wrapping_add(1).max(1);
        if let Err(e) = self.stream.write_all(&self.serialize_buf) {
            self.poisoned = true;
            return Err(ClientError::Io(e));
        }

        loop {
            let mut result = None;
            while let Some(res) = self.reader.next_frame() {
                match res {
                    Ok((header, body)) if header.req_id == req_id && header.msg_type == R::Reply::TYPE => {
                        log_debug!("Client: got reply id {} - {:?}({})", req_id, R::Reply::TYPE, body.len());
                        result = Some(Ok(decode_body::<R::Reply>(body)?));
                    }
                    Err(e) => {
                        self.poisoned = true;
                        return Err(ClientError::Protocol(e));
                    }
                    Ok((header, body)) => match header.msg_type {
                        MessageType::Error if header.req_id == req_id => {
                            let err = decode_body::<ErrorReply>(body)?;
                            result = Some(Err(ClientError::ResponseError(err)));
                        }
                        MessageType::Error if header.req_id == 0 => {
                            let err = decode_body::<ErrorReply>(body)?;
                            result = Some(Err(ClientError::ConnectionError(err)));
                        }
                        MessageType::Event(event_type) => {
                            log_debug!("Client: got event type {:?}({})", event_type, body.len());
                            self.events.push_back((event_type, body.into()));
                        }
                        MessageType::Unknown(t) if header.req_id == req_id => {
                            result = Some(Err(ClientError::UnexpectedType(MessageType::from_wire_u16(t))));
                        }
                        MessageType::Unknown(t) => {
                            log_debug!("Unknown event type {:?}", MessageType::from_wire_u16(t));
                            continue;
                        }
                        _ => continue,
                    },
                }
            }

            if let Some(r) = result {
                return r;
            }

            match self.reader.fill_from(&mut self.stream) {
                Ok(0) => {
                    self.poisoned = true;
                    return Err(ClientError::EofClosed);
                }
                Ok(_) => {}
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
