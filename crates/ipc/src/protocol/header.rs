pub const HEADER_LEN: usize = 12;
pub const MAX_REQUEST_LEN: u32 = 64 * 1024;
pub const MAX_FRAME_LEN: u32 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RequestType {
    Hello = 1,
    Status,
    List,
    Get,
    Restore,
    Pin,
    Delete,
    Clear,
    Subscribe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EventType {
    Added = 1,
    Updated,
    Removed,
    Cleared,
    SelectionChanged,
}

const ALL_REQ_TYPES: [Option<RequestType>; u8::MAX as usize + 1] = make_req_types();
const ALL_EV_TYPES: [Option<EventType>; u8::MAX as usize + 1] = make_ev_types();

const MSG_REQUEST: u8 = 1;
const MSG_REPLY: u8 = 2;
const MSG_ERROR: u8 = 3;
const MSG_EVENT: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Request(RequestType),
    Reply(RequestType),
    Error,
    Event(EventType),
    Unknown(u16),
}

impl MessageType {
    pub fn from_wire(category: u8, index: u8) -> Self {
        match (category, ALL_REQ_TYPES[index as usize], ALL_EV_TYPES[index as usize]) {
            (MSG_REQUEST, Some(r), _) => Self::Request(r),
            (MSG_REPLY, Some(r), _) => Self::Reply(r),
            (MSG_ERROR, _, _) if index == 0 => Self::Error,
            (MSG_EVENT, _, Some(e)) => Self::Event(e),
            _ => Self::Unknown((category as u16) << 8 | index as u16),
        }
    }

    pub fn from_wire_u16(t: u16) -> Self {
        let [category, index] = t.to_be_bytes();
        Self::from_wire(category, index)
    }

    pub fn to_wire(self) -> [u8; 2] {
        match self {
            Self::Request(r) => [MSG_REQUEST, r as u8],
            Self::Reply(r) => [MSG_REPLY, r as u8],
            Self::Error => [MSG_ERROR, 0],
            Self::Event(e) => [MSG_EVENT, e as u8],
            Self::Unknown(n) => n.to_be_bytes(),
        }
    }
}

pub struct Header {
    pub len: u32,
    pub msg_type: MessageType,
    pub nfds: u8,
    pub flags: u8,
    pub req_id: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0; HEADER_LEN];
        b[0..4].copy_from_slice(&self.len.to_le_bytes());
        b[4..6].copy_from_slice(&self.msg_type.to_wire());
        b[6] = self.nfds;
        b[7] = self.flags;
        b[8..12].copy_from_slice(&self.req_id.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; HEADER_LEN]) -> Self {
        let [l0, l1, l2, l3, msg_category, msg_index, nfds, flags, r0, r1, r2, r3] = *b;
        Self {
            len: u32::from_le_bytes([l0, l1, l2, l3]),
            msg_type: MessageType::from_wire(msg_category, msg_index),
            nfds,
            flags,
            req_id: u32::from_le_bytes([r0, r1, r2, r3]),
        }
    }
}

const fn make_req_types() -> [Option<RequestType>; u8::MAX as usize + 1] {
    let mut arr = [None; u8::MAX as usize + 1];

    arr[RequestType::Hello as usize] = Some(RequestType::Hello);
    arr[RequestType::Status as usize] = Some(RequestType::Status);
    arr[RequestType::List as usize] = Some(RequestType::List);
    arr[RequestType::Get as usize] = Some(RequestType::Get);
    arr[RequestType::Restore as usize] = Some(RequestType::Restore);
    arr[RequestType::Pin as usize] = Some(RequestType::Pin);
    arr[RequestType::Delete as usize] = Some(RequestType::Delete);
    arr[RequestType::Clear as usize] = Some(RequestType::Clear);
    arr[RequestType::Subscribe as usize] = Some(RequestType::Subscribe);

    arr
}

const fn make_ev_types() -> [Option<EventType>; u8::MAX as usize + 1] {
    let mut arr = [None; u8::MAX as usize + 1];

    arr[EventType::Added as usize] = Some(EventType::Added);
    arr[EventType::Updated as usize] = Some(EventType::Updated);
    arr[EventType::Removed as usize] = Some(EventType::Removed);
    arr[EventType::Cleared as usize] = Some(EventType::Cleared);
    arr[EventType::SelectionChanged as usize] = Some(EventType::SelectionChanged);

    arr
}
