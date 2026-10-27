use std::{
    hash::{BuildHasher, Hasher, RandomState},
    io,
    os::fd::OwnedFd,
    sync::Arc,
};

use clipleaf_core::{MimeKind, WlDataControlOffer, WlDataControlSource, WlSessionManager, mime_types::TEXT_MIME_TYPES};

use crate::{offer::serve::ServeJob, storage::Entry};

pub mod offers_fetch;
pub mod serve;

/// Upper bound on the types fetched per copy; the default pick yields at most 6 (text + image + 4 file lists).
pub const MAX_FETCHED_TYPES: usize = 10;

pub const RESTORE_ON_START: bool = true;
pub const RESTORE_ON_CLEAR: bool = true;
pub const TRACK_PRIMARY_SELECTION: bool = false;
pub const MAX_PENDING_SERVES: usize = 64;

pub struct OwnSource {
    source: WlDataControlSource,
    entry: Arc<Entry>,
}

pub struct DataOwner {
    marker: String,
    clipboard: Option<OwnSource>,
    primary: Option<OwnSource>,
}

impl DataOwner {
    pub fn new() -> Self {
        Self {
            marker: format!("x-clipleaf/{:08x}", RandomState::new().build_hasher().finish() as u32),
            clipboard: None,
            primary: None,
        }
    }

    pub fn is_ours(&self, offer: &WlDataControlOffer) -> bool {
        offer.offered_mime_types().iter().any(|t| *t == self.marker)
    }

    pub fn restore(&mut self, s: &mut WlSessionManager, entry: Arc<Entry>, primary: bool) -> io::Result<()> {
        let source = s.create_data_source()?;

        let res = {
            let mut offers = Vec::<&str>::new();

            for t in &entry.types {
                let mime = t.mime.mime().as_ref();
                if t.mime.kind() == MimeKind::Text {
                    for t in TEXT_MIME_TYPES.iter() {
                        if !offers.contains(t) {
                            offers.push(*t);
                        }
                    }
                } else if !offers.contains(&mime) {
                    offers.push(mime);
                }
            }

            for o in offers {
                source.offer(s.get_message_writer(), o)?;
            }
            source.offer(s.get_message_writer(), &self.marker)?;

            match primary {
                true => {
                    s.set_primary_selection(source.local_id)?;
                }
                false => {
                    s.set_selection(source.local_id)?;
                }
            }

            Ok(())
        };

        if let Err(e) = res {
            // in case the offer has been created and something fails after - we need to cleanup and destroy the source to avoid leaking a slot.
            let _ = s.destroy_source(source);
            return Err(e);
        }

        let slot = if primary { &mut self.primary } else { &mut self.clipboard };
        if let Some(old) = slot.replace(OwnSource { source, entry }) {
            s.destroy_source(old.source)?;
        }
        Ok(())
    }

    pub fn on_cancelled(&mut self, s: &mut WlSessionManager, local_source_id: u32) -> io::Result<()> {
        let Some(source) = self
            .clipboard
            .take_if(|x| x.source.local_id == local_source_id)
            .or_else(|| self.primary.take_if(|x| x.source.local_id == local_source_id))
        else {
            return Ok(());
        };

        s.destroy_source(source.source)
    }

    pub fn on_send(&self, local_source_id: u32, mime: &[u8], fd: OwnedFd) -> Option<ServeJob> {
        let mut source: Option<(&OwnSource, bool)> = None;
        if let Some(ref s) = self.clipboard
            && s.source.local_id == local_source_id
        {
            source = Some((s, false));
        } else if let Some(ref s) = self.primary
            && s.source.local_id == local_source_id
        {
            source = Some((s, true));
        }
        let (source, is_primary) = source?;
        let idx = source.entry.types.iter().position(|x| {
            x.mime.mime().as_bytes() == mime || (x.mime.kind() == MimeKind::Text && TEXT_MIME_TYPES.iter().any(|a| a.as_bytes() == mime))
        })?;

        Some(ServeJob {
            entry: source.entry.clone(),
            type_index: idx,
            fd,
            is_primary,
        })
    }
}
