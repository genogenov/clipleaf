use std::{
    io,
    marker::PhantomData,
    sync::{Arc, mpsc::Sender},
};

use clipleaf_core::ffi::event_fd::EventFd;
use clipleaf_ipc::protocol::{
    Request,
    codec::encode_frame,
    error::ProtocolError,
    handshake::{ErrorCode, ErrorReply, Hello, HelloReply},
    header::MAX_FRAME_LEN,
};

pub type EncoderFn = Box<dyn FnOnce(&mut Vec<u8>) -> Result<(), ProtocolError> + Send>;

#[allow(dead_code)]
pub struct ReplyWaitHandle<T>
where
    T: Request,
    T::Reply: 'static + Send,
{
    reply_ready: Arc<EventFd>,
    reply_tx: Option<Sender<(u32, EncoderFn)>>,
    req_id: u32,
    _marker: PhantomData<T>,
}

#[allow(dead_code)]
impl<T: Request> ReplyWaitHandle<T>
where
    T: Request,
    T::Reply: 'static + Send,
{
    pub fn new(ev: Arc<EventFd>, tx: Sender<(u32, EncoderFn)>, req_id: u32) -> Self {
        Self {
            reply_ready: ev,
            reply_tx: Some(tx),
            req_id,
            _marker: PhantomData,
        }
    }

    pub fn signal_reply(mut self, reply: Result<T::Reply, ErrorReply>) -> io::Result<()> {
        self.push(reply)?;
        self.reply_ready.signal()
    }

    pub fn set_reply(mut self, reply: Result<T::Reply, ErrorReply>) -> io::Result<()> {
        self.push(reply)
    }

    fn push(&mut self, reply: Result<T::Reply, ErrorReply>) -> io::Result<()> {
        let Some(tx) = self.reply_tx.take() else {
            return Ok(());
        };
        let req_id = self.req_id;
        let encoder_fn = move |vec: &mut Vec<u8>| match reply {
            Ok(ok) => encode_frame(req_id, &ok, vec, MAX_FRAME_LEN),
            Err(e) => encode_frame(req_id, &e, vec, MAX_FRAME_LEN),
        };
        tx.send((self.req_id, Box::new(encoder_fn)))
            .map_err(|e| io::Error::other(format!("Failed to send response to IPC connection: {e}")))?;
        Ok(())
    }
}

impl<T: Request> Drop for ReplyWaitHandle<T>
where
    T: Request,
    T::Reply: 'static + Send,
{
    fn drop(&mut self) {
        if self.reply_tx.is_some() {
            let _ = self.push(Err(ErrorReply {
                code: ErrorCode::Internal,
                message: "request dropped".into(),
            }));
            let _ = self.reply_ready.signal();
        }
    }
}

pub trait RequestMessageHandler: Send + Clone {
    fn handle_hello(&mut self, msg: Hello) -> Result<HelloReply, ErrorReply>;
}
