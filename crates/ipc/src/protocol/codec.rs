use crate::protocol::{
    Message,
    error::ProtocolError,
    header::{HEADER_LEN, Header},
};

pub fn encode_frame<T: Message>(req_id: u32, message: &T, buffer: &mut Vec<u8>, max_allowed_len: usize) -> Result<(), ProtocolError> {
    let start = buffer.len();
    buffer.extend_from_slice(&[0; HEADER_LEN]);
    if let Err(e) = rmp_serde::encode::write_named(buffer, message) {
        buffer.truncate(start);
        return Err(e.into());
    }
    let frame_len = buffer.len() - start;
    if frame_len > max_allowed_len {
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
    let msg = rmp_serde::decode::from_slice::<T>(body)?;
    Ok(msg)
}
