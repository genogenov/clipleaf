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
        msg.pack_fd(fd)?;
        msg.end();
        Ok(())
    }

    pub fn destroy(id: u32, writer: WlMessageWriter) -> Result<(), std::io::Error> {
        let msg = writer.begin_message::<WlDataControlOffer>(WlDataControlOfferOps::Destroy, id)?;
        msg.end();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fd_io::serve_bytes, wl::wl_buffered_stream::WlBufferedStream};
    use std::{io::Read, os::unix::net::UnixStream, time::Duration};

    fn offer_with(types: &[&[u8]]) -> WlDataControlOffer {
        let mut offer = WlDataControlOffer::new(0xff00_0000);
        for mime in types {
            offer.push_offer(mime);
        }
        offer
    }

    fn types(offer: &WlDataControlOffer) -> Vec<&str> {
        offer
            .offered_mime_types()
            .iter()
            .map(|m| m.as_ref())
            .collect()
    }

    fn preferred(list: &[&str]) -> Option<String> {
        let bytes: Vec<&[u8]> = list.iter().map(|m| m.as_bytes()).collect();
        offer_with(&bytes)
            .preferred_mime_type()
            .map(|m| m.to_string())
    }

    #[test]
    fn types_are_kept_in_order_without_duplicates_and_known_ones_do_not_allocate() {
        let offer = offer_with(&[b"image/png", b"text/plain", b"image/png", b"text/plain"]);
        assert_eq!(types(&offer), ["image/png", "text/plain"]);
        assert!(matches!(offer.offered_mime_types()[0], Cow::Owned(_)));
        assert!(matches!(offer.offered_mime_types()[1], Cow::Borrowed(_)));
    }

    #[test]
    fn invalid_types_are_skipped_and_the_rest_still_recorded() {
        let offer = offer_with(&[b"text/\x1b[31m", b"", b"text/\xff", b"text/html"]);
        assert_eq!(types(&offer), ["text/html"]);
    }

    #[test]
    fn types_beyond_the_cap_are_skipped() {
        let names: Vec<String> = (0..MAX_OFFERED_MIME_TYPES + 5)
            .map(|i| format!("application/x-{i}"))
            .collect();
        let bytes: Vec<&[u8]> = names.iter().map(|m| m.as_bytes()).collect();
        let offer = offer_with(&bytes);
        assert_eq!(offer.offered_mime_types().len(), MAX_OFFERED_MIME_TYPES);
        assert_eq!(
            offer.offered_mime_types().last().map(|m| m.as_ref()),
            Some(names[MAX_OFFERED_MIME_TYPES - 1].as_str())
        );
    }

    #[test]
    fn preferred_type_is_best_known_text_then_other_text_then_first_type() {
        assert_eq!(
            preferred(&["text/html", "STRING", "UTF8_STRING", "image/png"]).as_deref(),
            Some("UTF8_STRING")
        );
        assert_eq!(
            preferred(&["image/png", "text/uri-list"]).as_deref(),
            Some("text/uri-list")
        );
        assert_eq!(
            preferred(&["image/png", "image/jpeg"]).as_deref(),
            Some("image/png")
        );
        assert_eq!(preferred(&[]), None);
    }

    #[test]
    fn asked_type_matches_exactly() {
        let offer = offer_with(&[b"text/plain;charset=utf-8", b"image/png"]);
        assert_eq!(
            offer.asked_mime_type("image/png").map(|m| m.as_ref()),
            Some("image/png")
        );
        for missing in ["image", "text/plain", "IMAGE/PNG"] {
            assert!(offer.asked_mime_type(missing).is_none(), "{missing}");
        }
    }

    #[test]
    fn receive_sends_the_mime_and_the_fd_and_destroy_has_no_arguments() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut client = WlBufferedStream::new(a.into());
        let mut compositor = WlBufferedStream::new(b.into());
        let offer_id: u32 = 0xff00_0000;
        let (mut payload_rx, payload_tx) = UnixStream::pair().unwrap();

        WlDataControlOffer::receive(
            offer_id,
            client.get_writer(),
            "text/plain",
            payload_tx.into(),
        )
        .unwrap();
        WlDataControlOffer::destroy(offer_id, client.get_writer()).unwrap();
        client.write().unwrap();

        let (header, body, fds) = compositor.read_next_message().unwrap().unwrap();
        assert_eq!(
            (header.object_id, header.opcode),
            (offer_id, u16::from(WlDataControlOfferOps::Receive))
        );
        assert_eq!(WlMessageReader::new(body).str(), Some(&b"text/plain"[..]));
        serve_bytes(fds.pop_last_in_fd().unwrap(), b"hello").unwrap();

        let (header, ..) = compositor.read_next_message().unwrap().unwrap();
        assert_eq!(
            (header.object_id, header.opcode, header.size),
            (offer_id, u16::from(WlDataControlOfferOps::Destroy), 8)
        );

        // Times out instead of hanging if the client kept its copy of the write end.
        payload_rx
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut got = Vec::new();
        payload_rx.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"hello");
    }
}
