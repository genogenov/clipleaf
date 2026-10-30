use crate::protocol::{
    Message,
    error::ProtocolError,
    header::{HEADER_LEN, Header},
};

const MAX_BODY_DEPTH: usize = 32;

pub fn encode_frame<T: Message>(req_id: u32, message: &T, buffer: &mut Vec<u8>, max_allowed_len: u32) -> Result<(), ProtocolError> {
    let start = buffer.len();
    buffer.extend_from_slice(&[0; HEADER_LEN]);
    if let Err(e) = rmp_serde::encode::write_named(buffer, message) {
        buffer.truncate(start);
        return Err(e.into());
    }
    let frame_len = buffer.len() - start;
    if frame_len > max_allowed_len as usize {
        buffer.truncate(start);
        return Err(ProtocolError::FrameTooBig);
    }
    let header = Header {
        len: (frame_len - HEADER_LEN) as u32,
        msg_type: T::TYPE,
        nfds: 0,
        flags: 0,
        req_id,
    };
    buffer[start..start + HEADER_LEN].copy_from_slice(&header.encode());

    Ok(())
}

pub fn decode_body<T: Message>(body: &[u8]) -> Result<T, ProtocolError> {
    let mut de = rmp_serde::Deserializer::from_read_ref(body);
    de.set_max_depth(MAX_BODY_DEPTH);
    Ok(T::deserialize(&mut de)?)
}
