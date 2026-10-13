use std::{borrow::Cow, os::fd::OwnedFd};

use crate::{
    fd_io::InFdBuffer,
    log_debug, parse_mime,
    wl::{
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

pub const TEXT_PLAIN_UTF8: &str = "text/plain;charset=utf-8";
pub const UTF8_STRING: &str = "UTF8_STRING";
pub const TEXT_PLAIN: &str = "text/plain";
pub const STRING: &str = "STRING";
pub const TEXT: &str = "TEXT";
pub const TEXT_HTML: &str = "text/html";

// Ordered by preference: index == rank.
pub const KNOWN_MIME_TYPES: [&str; 6] = [
    TEXT_PLAIN_UTF8,
    UTF8_STRING,
    TEXT_PLAIN,
    STRING,
    TEXT,
    TEXT_HTML,
];
pub const MAX_OFFERED_MIME_TYPES: usize = 128;
pub const OFFERED_TXT_MIME_TYPES: [&str; 5] =
    [TEXT_PLAIN_UTF8, UTF8_STRING, TEXT_PLAIN, STRING, TEXT];

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

    pub fn preferred_mime_type(&self) -> Option<&Cow<'static, str>> {
        let ranks = self
            .offered_mime_types
            .iter()
            .map(|mime| {
                KNOWN_MIME_TYPES.iter().position(|m| m == mime).map_or_else(
                    || {
                        if mime.contains("text") {
                            (99, mime)
                        } else {
                            (999, mime)
                        }
                    },
                    |rank| (rank, mime),
                )
            })
            .min_by(|(rank_a, _), (rank_b, _)| rank_a.cmp(rank_b));

        ranks.map(|(_, mime)| mime)
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
            Err(_e) => {
                log_debug!("Failed to parse mime, skipping: '{}'", _e);
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
