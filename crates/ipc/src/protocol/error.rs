use std::{error, fmt::Display, io};

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    FrameTooBig,
    Encode(rmp_serde::encode::Error),
    Decode(rmp_serde::decode::Error),
}

impl From<io::Error> for ProtocolError {
    fn from(e: io::Error) -> Self {
        ProtocolError::Io(e)
    }
}

impl From<rmp_serde::encode::Error> for ProtocolError {
    fn from(e: rmp_serde::encode::Error) -> Self {
        ProtocolError::Encode(e)
    }
}

impl From<rmp_serde::decode::Error> for ProtocolError {
    fn from(e: rmp_serde::decode::Error) -> Self {
        ProtocolError::Decode(e)
    }
}

impl error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            ProtocolError::Io(error) => Some(error),
            ProtocolError::FrameTooBig => None,
            ProtocolError::Encode(error) => Some(error),
            ProtocolError::Decode(error) => Some(error),
        }
    }
}

impl Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::Io(error) => write!(f, "I/O Error: {error}"),
            ProtocolError::FrameTooBig => write!(f, "The frame is too big"),
            ProtocolError::Encode(error) => write!(f, "Rmp Serde encode error: {error}"),
            ProtocolError::Decode(error) => write!(f, "Rmp Serde decode error: {error}"),
        }
    }
}
