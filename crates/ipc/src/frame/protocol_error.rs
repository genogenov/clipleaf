use std::io;

pub enum ProtocolError {
    Io(io::Error),
    FrameTooBig,
}

impl From<io::Error> for ProtocolError {
    fn from(e: io::Error) -> Self {
        ProtocolError::Io(e)
    }
}
