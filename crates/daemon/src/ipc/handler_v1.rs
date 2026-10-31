use clipleaf_ipc::protocol::handshake::{ErrorCode, ErrorReply, Hello, HelloReply};

use crate::ipc::message_handler::RequestMessageHandler;

const SUPPORTED_VERSION: u32 = 1;
const SERVER_STRING: &str = concat!("clipleafd ", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct HandlerV1 {}

impl RequestMessageHandler for HandlerV1 {
    fn handle_hello(&mut self, msg: Hello) -> Result<HelloReply, ErrorReply> {
        if !msg.versions.contains(&SUPPORTED_VERSION) {
            return Err(ErrorReply {
                code: ErrorCode::UnsupportedVersion,
                message: format!("Supported versions: [{SUPPORTED_VERSION}]"),
            });
        }

        Ok(HelloReply {
            version: SUPPORTED_VERSION,
            features: Vec::new(),
            daemon: Some(SERVER_STRING.into()),
        })
    }
}
