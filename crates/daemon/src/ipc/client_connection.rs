use std::{
    io::{self, ErrorKind, Write},
    ops::Add,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use clipleaf_core::{
    ffi::{
        event_fd::EventFd,
        poll::{PollEvents, PollFd, poll},
    },
    log_debug, log_error, log_warn,
};
use clipleaf_ipc::{
    frame::frame_reader::FrameReader,
    protocol::{
        Request,
        codec::{decode_body, encode_frame},
        error::ProtocolError,
        handshake::{ErrorCode, ErrorReply, Hello},
        header::{Header, MAX_FRAME_LEN, MAX_REQUEST_LEN, MessageType, RequestType},
    },
};

use crate::ipc::message_handler::{EncoderFn, ReplyWaitHandle, RequestMessageHandler};

const MAX_BACKPRESSURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_REPLIES: usize = 16;
const BUF_SHRINK_THRESHOLD: usize = 1 << 21;
const BUF_SHRINK_SIZE: usize = 1 << 20;

#[derive(Debug, PartialEq, Eq)]
enum State {
    AwaitHandshake,
    Ready,
    Closing { deadline: Instant },
}

struct ReplySink {
    tx: Sender<(u32, EncoderFn)>,
    rx: Receiver<(u32, EncoderFn)>,
    ready: Arc<EventFd>,
    pending: usize,
}
impl ReplySink {
    #[allow(dead_code)]
    fn handle<R: Request>(&mut self, req_id: u32) -> ReplyWaitHandle<R>
    where
        R::Reply: Send,
    {
        self.pending += 1;
        ReplyWaitHandle::new(self.ready.clone(), self.tx.clone(), req_id)
    }
}

impl State {
    fn transition_closing(&mut self) {
        *self = State::Closing {
            deadline: Instant::now().add(Duration::from_secs(1)),
        };
    }
}

pub struct IpcClientConnection<T>
where
    T: RequestMessageHandler,
{
    stream: UnixStream,
    shutdown: Arc<EventFd>,
    reader: FrameReader,
    out_buf: Vec<u8>,
    out_pos: usize,
    state: State,
    handler: T,
    reply_sink: ReplySink,
}

impl<T: RequestMessageHandler> IpcClientConnection<T> {
    pub fn new(stream: UnixStream, shutdown: Arc<EventFd>, handler: T) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            stream,
            shutdown,
            reader: FrameReader::new(4096, MAX_REQUEST_LEN as usize),
            out_buf: Vec::with_capacity(4096),
            out_pos: 0,
            state: State::AwaitHandshake,
            handler,
            reply_sink: ReplySink {
                tx,
                rx,
                ready: Arc::new(EventFd::new()?),
                pending: 0,
            },
        })
    }

    pub fn run(&mut self) {
        loop {
            if matches!(self.state, State::Closing { .. }) && self.out_pos == self.out_buf.len() {
                log_debug!("Connection closing and out buffer drained - Closing.");
                break;
            }
            let mut stream_events = PollEvents::EMPTY;
            if self.out_buf.len() - self.out_pos < MAX_BACKPRESSURE_BYTES
                && self.reply_sink.pending < MAX_PENDING_REPLIES
                && !matches!(self.state, State::Closing { .. })
            {
                stream_events |= PollEvents::IN;
            }
            if self.out_buf.len() - self.out_pos > 0 {
                stream_events |= PollEvents::OUT;
            }
            let mut fds = [
                PollFd::new(self.stream.as_fd(), stream_events),
                PollFd::new(self.shutdown.as_fd(), PollEvents::IN),
                PollFd::new(self.reply_sink.ready.as_fd(), PollEvents::IN),
            ];
            let deadline = {
                if let State::Closing { deadline } = self.state {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        log_debug!("closing deadline passed; dropping connection");
                        break;
                    }
                    Some(remaining)
                } else {
                    None
                }
            };
            match poll(&mut fds, deadline) {
                Ok(_) => {
                    let (conn_ev, stop_ev, replies_ev) = (fds[0].revents(), fds[1].revents(), fds[2].revents());
                    if stop_ev.has_any(PollEvents::IN) {
                        log_debug!("Client connection exiting.");
                        break;
                    }

                    if conn_ev.has_any(PollEvents::ERR | PollEvents::NVAL) || (conn_ev.has_any(PollEvents::HUP) && !conn_ev.has_any(PollEvents::IN)) {
                        log_debug!("Poll returned errors, or hung up - dropping connection");
                        break;
                    }

                    if conn_ev.has_any(PollEvents::OUT)
                        && let Err(e) = self.write_to_stream()
                    {
                        if e.kind() != ErrorKind::BrokenPipe {
                            log_error!("Failed to write to stream: {e}. Dropping connection.");
                        }
                        break;
                    }

                    if conn_ev.has_any(PollEvents::IN) {
                        match self.fill_read_buffer() {
                            Ok(Some(_)) => {
                                if let Err(e) = self.read_from_buffer() {
                                    log_error!("Failed to read frame: {e}. Dropping connection.");
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                log_error!("Failed to fill read buffer: {e}. Dropping connection.");
                                break;
                            }
                        }
                    }

                    if replies_ev.has_any(PollEvents::IN) {
                        if let Err(e) = self.reply_sink.ready.drain() {
                            log_error!("Failed to reset pending reply signal: {e}. Dropping conection");
                            break;
                        }
                        self.handle_pending_replies();
                        if let Err(e) = self.read_from_buffer() {
                            log_error!("Failed to read frame: {e}. Dropping connection.");
                            break;
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => {
                    log_error!("IPC client connection: poll failed: {e}");
                    break;
                }
            }

            if let Err(e) = self.write_to_stream() {
                if e.kind() != ErrorKind::BrokenPipe {
                    log_error!("Failed to write to stream: {e}. Dropping connection.");
                }
                break;
            }
        }
    }

    fn write_to_stream(&mut self) -> io::Result<()> {
        while self.out_pos < self.out_buf.len() {
            match self.stream.write(&self.out_buf[self.out_pos..]) {
                Ok(0) => {
                    return Err(io::Error::new(ErrorKind::WriteZero, "Could not write any bytes to stream."));
                }
                Ok(n) => {
                    self.out_pos += n;
                }
                Err(e) => match e.kind() {
                    ErrorKind::WouldBlock => return Ok(()),
                    ErrorKind::Interrupted => {}
                    _ => {
                        return Err(e);
                    }
                },
            }
        }

        self.out_buf.clear();
        if self.out_buf.capacity() > BUF_SHRINK_THRESHOLD {
            self.out_buf.shrink_to(BUF_SHRINK_SIZE);
        }
        self.out_pos = 0;
        Ok(())
    }

    fn fill_read_buffer(&mut self) -> io::Result<Option<usize>> {
        loop {
            match self.reader.fill_from(&mut self.stream) {
                Ok(0) => {
                    return Err(io::Error::new(
                        ErrorKind::UnexpectedEof,
                        "Reader returned 0 bytes - client connection closed",
                    ));
                }
                Ok(n) => {
                    return Ok(Some(n));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock) => return Ok(None),
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }

    fn read_from_buffer(&mut self) -> Result<(), ProtocolError> {
        while self.reply_sink.pending < MAX_PENDING_REPLIES
            && !matches!(self.state, State::Closing { .. })
            && let Some(res) = self.reader.next_frame()
        {
            let (header, body) = res?;
            Self::handle_frame(&mut self.out_buf, &mut self.state, &mut self.handler, &mut self.reply_sink, &header, body)?;
            self.handle_pending_replies();
            if matches!(self.state, State::Closing { .. }) {
                break;
            }
        }

        Ok(())
    }

    fn handle_pending_replies(&mut self) {
        while let Ok((req_id, encoder)) = self.reply_sink.rx.try_recv() {
            let start = self.out_buf.len();
            if let Err(e) = encoder(&mut self.out_buf) {
                self.out_buf.truncate(start);
                let _ = Self::write_error(&mut self.out_buf, req_id, ErrorCode::Internal, format!("encode failed: {e}"));
            }
            self.reply_sink.pending -= 1;
        }
    }

    fn handle_frame(
        out_buf: &mut Vec<u8>,
        state: &mut State,
        handler: &mut T,
        sink: &mut ReplySink,
        header: &Header,
        body: &[u8],
    ) -> Result<(), ProtocolError> {
        match header.msg_type {
            MessageType::Request(request_type) => Self::handle_request(state, out_buf, handler, (header, body, request_type), sink),
            MessageType::Reply(_) | MessageType::Error | MessageType::Event(_) => {
                log_warn!("Received unsupported message type: {:?}. Closing the connection.", header.msg_type);
                Self::write_error(
                    out_buf,
                    0,
                    ErrorCode::BadRequest,
                    format!("Unsupported request type: {:?}", header.msg_type),
                )?;
                state.transition_closing();
                Ok(())
            }
            MessageType::Unknown(n) if *state != State::AwaitHandshake && n >> 8 == 1 => {
                log_warn!("Received unknown message type for category 'Request': {:?}", header.msg_type);
                Self::write_error(
                    out_buf,
                    header.req_id,
                    ErrorCode::UnknownRequest,
                    format!("Unknown request type: {:?}", header.msg_type),
                )
            }
            MessageType::Unknown(_) => {
                log_warn!("Received unknown message type: {:?}", header.msg_type);
                state.transition_closing();
                Self::write_error(out_buf, 0, ErrorCode::BadRequest, format!("Unknown request type: {:?}", header.msg_type))
            }
        }
    }

    fn write_error(out_buf: &mut Vec<u8>, req_id: u32, code: ErrorCode, message: String) -> Result<(), ProtocolError> {
        let err = ErrorReply { code, message };
        encode_frame(req_id, &err, out_buf, MAX_FRAME_LEN)
    }

    fn handle_request(
        state: &mut State,
        out_buf: &mut Vec<u8>,
        handler: &mut T,
        req: (&Header, &[u8], RequestType),
        _sink: &mut ReplySink,
    ) -> Result<(), ProtocolError> {
        if *state == State::AwaitHandshake && req.2 != RequestType::Hello {
            Self::write_error(out_buf, 0, ErrorCode::BadRequest, "The handshake message must be Hello.".to_string())?;
            state.transition_closing();
            return Ok(());
        }

        match req.2 {
            RequestType::Hello => {
                if *state != State::AwaitHandshake {
                    Self::write_error(
                        out_buf,
                        req.0.req_id,
                        ErrorCode::BadRequest,
                        "Hello message can only be sent as the first handshake message.".to_string(),
                    )?;
                    return Ok(());
                }

                let Ok(msg) = decode_body::<Hello>(req.1) else {
                    Self::write_error(out_buf, 0, ErrorCode::BadRequest, "Coulld not decode Hello msg body".into())?;
                    state.transition_closing();
                    return Ok(());
                };
                log_debug!("Got Hello msg: {msg:?}");

                match handler.handle_hello(msg) {
                    Ok(msg) => {
                        log_debug!("Sending Hello reply: {msg:?}");
                        encode_frame(req.0.req_id, &msg, out_buf, MAX_FRAME_LEN)?;
                        *state = State::Ready;
                    }
                    Err(e) => {
                        Self::write_error(out_buf, 0, e.code, e.message)?;
                        state.transition_closing();
                    }
                }
                Ok(())
            }
            RequestType::Status => todo!(),
            RequestType::List => todo!(),
            RequestType::Get => todo!(),
            RequestType::Restore => todo!(),
            RequestType::Pin => todo!(),
            RequestType::Delete => todo!(),
            RequestType::Clear => todo!(),
            RequestType::Subscribe => todo!(),
        }
    }
}
