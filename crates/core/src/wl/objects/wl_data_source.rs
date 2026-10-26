use std::os::fd::OwnedFd;

use crate::{
    fd_io::InFdBuffer,
    wl::{
        objects::{WlObject, wl_enum},
        wl_message_reader::WlMessageReader,
        wl_message_writer::WlMessageWriter,
    },
};

pub struct WlDataControlSource {
    pub local_id: u32,
}

impl WlObject for WlDataControlSource {
    type Ops = WlDataControlSourceOps;
    type Events = WlDataControlSourceEvents;
}

pub enum WlDataControlSourceEvents {
    Send = 0,
    Cancelled = 1,
}

wl_enum! {
    pub enum WlDataControlSourceOps {
        Offer = 0,
        Destroy = 1,
    }
}

pub enum WlDataControlSourceEvent<'a> {
    Send { mime_type: &'a [u8], fd: OwnedFd },
    Cancelled,
}

impl WlDataControlSource {
    pub fn offer(&self, writer: WlMessageWriter, mime_type: &str) -> std::io::Result<()> {
        let mut msg = writer
            .begin_message::<WlDataControlSource>(WlDataControlSourceOps::Offer, self.local_id)?;
        msg.pack_str(mime_type)?;
        msg.end()
    }

    pub fn parse_message<'a>(
        opcode: u16,
        buffer: &'a [u8],
        fds: &mut InFdBuffer,
    ) -> std::io::Result<WlDataControlSourceEvent<'a>> {
        if opcode == WlDataControlSourceEvents::Send as u16 {
            let mut reader = WlMessageReader::new(buffer);
            let mime_type = reader
                .str()
                .ok_or_else(|| std::io::Error::other("Failed to read mime type"))?;
            return Ok(WlDataControlSourceEvent::Send {
                mime_type,
                fd: fds
                    .pop_in_fd()
                    .ok_or_else(|| std::io::Error::other("Failed to pop file descriptor"))?,
            });
        } else if opcode == WlDataControlSourceEvents::Cancelled as u16 {
            return Ok(WlDataControlSourceEvent::Cancelled);
        }
        Err(std::io::Error::other("Unknown opcode"))
    }

    pub(crate) fn destroy(self, writer: WlMessageWriter) -> std::io::Result<()> {
        let msg = writer
            .begin_message::<WlDataControlSource>(WlDataControlSourceOps::Destroy, self.local_id)?;
        msg.end()
    }
}
