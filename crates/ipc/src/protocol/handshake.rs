use std::fmt::Display;

use serde::{Deserialize, Serialize};

use crate::protocol::header::{MessageType, RequestType};

// ---- Handshake and errors ----

/// Must be the first frame on a connection, in both directions.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub versions: Vec<u32>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HelloReply {
    pub version: u32,
    #[serde(default)]
    pub features: Vec<String>,
    /// e.g. "clipleafd 0.1.0", for diagnostics.
    pub daemon: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ErrorReply {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
}

impl Display for ErrorReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "(Error {:?}):{}", self.code, self.message)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UnknownRequest,
    BadRequest,
    NotFound,
    TooManyConnections,
    UnsupportedVersion,
    Internal,
    Cancelled,
    #[serde(other)]
    Generic,
}

message! {
    Hello => MessageType::Request(RequestType::Hello);
    HelloReply => MessageType::Reply(RequestType::Hello);
    ErrorReply => MessageType::Error;
}

request! {
    Hello => HelloReply;
}
