use serde::{Serialize, de::DeserializeOwned};

use crate::protocol::header::MessageType;

pub trait Message: Serialize + DeserializeOwned {
    const TYPE: MessageType;
}
pub trait Request: Message {
    type Reply: Message;
}

macro_rules! message {
    ($($ty:ty => $msg_type:expr;)*) => {
        $(impl crate::protocol::Message for $ty {
            const TYPE: MessageType = $msg_type;
        })*
    };
}

macro_rules! request {
    ($($req:ty => $reply:ty;)*) => {
        $(impl crate::protocol::Request for $req {
            type Reply = $reply;
        })*
    };
}

pub mod codec;
pub mod handshake;
pub mod header;
pub mod v1;
