use std::{error, fmt::Display, io};

use crate::protocol::{error::ProtocolError, handshake::ErrorReply, header::MessageType};

#[derive(Debug)]
pub enum ClientError {
    UnsupportedVersion,
    Io(io::Error),
    Protocol(ProtocolError),
    UnexpectedType(MessageType),
    EofClosed,
    ResponseError(ErrorReply),
    ConnectionError(ErrorReply),
    Poisoned,
    Timeout,
    EventQueueFull,
}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl From<ProtocolError> for ClientError {
    fn from(e: ProtocolError) -> Self {
        ClientError::Protocol(e)
    }
}

impl error::Error for ClientError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            ClientError::Io(error) => Some(error),
            ClientError::Protocol(protocol_error) => Some(protocol_error),
            ClientError::EofClosed => None,
            ClientError::UnexpectedType(_) => None,
            ClientError::ResponseError(_) => None,
            ClientError::ConnectionError(_) => None,
            ClientError::UnsupportedVersion => None,
            ClientError::Poisoned => None,
            ClientError::Timeout => None,
            ClientError::EventQueueFull => None,
        }
    }
}

impl Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(error) => write!(f, "IPC Client I/O Error: {error}"),
            ClientError::Protocol(protocol_error) => write!(f, "IPC Client protocol error: {protocol_error}"),
            ClientError::EofClosed => write!(f, "IPC Client unexpected EoF: The remote closed the connection."),
            ClientError::UnexpectedType(msg_type) => write!(f, "IPC Client unexpected response message type: {:?}", msg_type),
            ClientError::ResponseError(error_reply) => write!(f, "IPC Client response error: {error_reply}"),
            ClientError::ConnectionError(error_reply) => write!(f, "IPC Client connection error: {error_reply}"),
            ClientError::UnsupportedVersion => write!(f, "IPC CLient got unsupported version from server"),
            ClientError::Poisoned => write!(f, "IPC CLient connection is poisoned. Drop the client and reconnect."),
            ClientError::Timeout => write!(f, "IPC CLient I/O timed out."),
            ClientError::EventQueueFull => write!(
                f,
                "IPC Client event queue is full and events were dropped. Client should fetch fresh snapshot(s)"
            ),
        }
    }
}
