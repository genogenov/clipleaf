use std::{borrow::Cow, os::fd::OwnedFd};

use crate::{
    fd_io::InFdBuffer, log_debug, mime_types::KNOWN_MIME_TYPES, parse_mime, wl::{
        objects::{WlObject, wl_enum},
        wl_message_reader::WlMessageReader,
        wl_message_writer::WlMessageWriter,
    },
};

const OFFER: u16 = 0;

wl_enum! {
    pub enum WlDataControlOfferEvents {
        Offer = OFFER
    }
}

wl_enum! {
    pub enum WlDataControlOfferOps {
        Receive = 0,

        Destroy = 1,
    }
}

pub enum DataControlOfferEvent<'a> {
    Offer { mime: &'a [u8] },
}

pub const MAX_OFFERED_MIME_TYPES: usize = 128;

pub struct WlDataControlOffer {
    id: u32,
    offered_mime_types: Vec<Cow<'static, str>>,
}
impl WlObject for WlDataControlOffer {
    type Ops = WlDataControlOfferOps;
    type Events = WlDataControlOfferEvents;
}

impl WlDataControlOffer {
    pub fn new(local_id: u32) -> Self {
        Self {
            id: local_id,
            offered_mime_types: Vec::with_capacity(8),
        }
    }

    pub fn asked_mime_type(&self, asked: &str) -> Option<&Cow<'static, str>> {
        self.offered_mime_types.iter().find(|m| *m == asked)
    }

    pub fn offered_mime_types(&self) -> &[Cow<'static, str>] {
        &self.offered_mime_types
    }

    pub fn push_offer(&mut self, mime: &[u8]) {
        if self.offered_mime_types.len() >= MAX_OFFERED_MIME_TYPES {
            log_debug!("Too many offered mime types, skipping additional offers");
            return;
        }

        let parsed_mime = match parse_mime(mime) {
            Ok(m) => m,
            Err(e) => {
                log_debug!("Failed to parse mime, skipping: '{}'", e);
                return;
            }
        };

        if self.offered_mime_types.iter().any(|m| m == parsed_mime) {
        } else if let Some(known) = KNOWN_MIME_TYPES.iter().find(|m| **m == parsed_mime) {
            self.offered_mime_types.push(Cow::Borrowed(known));
        } else {
            self.offered_mime_types
                .push(Cow::Owned(parsed_mime.to_owned()))
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn receive(
        id: u32,
        writer: WlMessageWriter,
        mime: &str,
        fd: OwnedFd,
    ) -> Result<(), std::io::Error> {
        let mut msg =
            writer.begin_message::<WlDataControlOffer>(WlDataControlOfferOps::Receive, id)?;
        msg.pack_str(mime)?;
        msg.end_with_fd(fd)?;
        Ok(())
    }

    pub fn destroy(id: u32, writer: WlMessageWriter) -> Result<(), std::io::Error> {
        let msg = writer.begin_message::<WlDataControlOffer>(WlDataControlOfferOps::Destroy, id)?;
        msg.end()?;
        Ok(())
    }

    pub fn parse_message<'a>(
        opcode: u16,
        buffer: &'a [u8],
        _fds: &mut InFdBuffer,
    ) -> std::io::Result<DataControlOfferEvent<'a>> {
        let mut reader = WlMessageReader::new(buffer);
        match opcode {
            OFFER => {
                let mime = reader.str().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to read mime")
                })?;
                Ok(DataControlOfferEvent::Offer { mime })
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unknown opcode",
            )),
        }
    }
}

impl PartialEq for WlDataControlOffer {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
