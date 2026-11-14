use std::{io, sync::Arc};

use clipleaf_core::{ContentKind, ContentKinds};
use clipleaf_ipc::protocol::{
    handshake::{ErrorCode, ErrorReply, Hello, HelloReply},
    v1::models::{EntrySummary, Kind, List, ListReply, Thumb, TypeSummary},
};

use crate::{
    index::{Item, SnapshotHandle},
    ipc::message_handler::{ReplyWaitHandle, RequestMessageHandler},
    storage::{Data, Entry, TypeMeta},
};

const SUPPORTED_VERSION: u32 = 1;
const SERVER_STRING: &str = concat!("clipleafd ", env!("CARGO_PKG_VERSION"));

#[derive(Clone)]
pub struct HandlerV1 {
    snapshot: SnapshotHandle,
}

impl HandlerV1 {
    pub fn new(snapshot: SnapshotHandle) -> Self {
        Self { snapshot }
    }
}

impl RequestMessageHandler for HandlerV1 {
    fn handle_hello(&mut self, msg: Hello) -> Result<HelloReply, ErrorReply> {
        if !msg.versions.contains(&SUPPORTED_VERSION) {
            return Err(ErrorReply {
                code: ErrorCode::UnsupportedVersion,
                message: format!("Supported versions: [{SUPPORTED_VERSION}]"),
            });
        }

        Ok(HelloReply {
            version: SUPPORTED_VERSION,
            features: Vec::new(),
            daemon: Some(SERVER_STRING.into()),
        })
    }

    fn list(&mut self, msg: List, reply: ReplyWaitHandle<List>) -> io::Result<()> {
        let snapshot = self.snapshot.get_clone();

        if msg.query.is_some() {
            todo!()
        }

        let entries = snapshot
            .items
            .iter()
            .skip(msg.offset as usize)
            .take(msg.limit.unwrap_or(100).min(100) as usize)
            .map(build_summary);

        reply.set_reply(Ok(ListReply {
            entries: entries.collect(),
            total: snapshot.items.len() as u64,
        }))
    }
}

fn build_summary(item: &Item) -> EntrySummary {
    let mut size = 0;
    let mut kind = None;
    let mut files_count = None;
    let mut preview = None;
    let mut thumb = None;
    let mut types = Vec::with_capacity(item.entry.types.len());
    for t in &item.entry.types {
        size += t.size;
        files_count = match t.meta {
            Some(TypeMeta::FileList { items }) => Some(items),
            _ => None,
        };

        let content_kind = ContentKind::from_mime(&t.mime);
        if kind.is_none() {
            kind = match content_kind {
                Some(ContentKind::File) => Some(Kind::File),
                Some(ContentKind::Html) => Some(Kind::Html),
                Some(ContentKind::Image) => Some(Kind::Image),
                Some(ContentKind::Markdown) => Some(Kind::Markdown),
                Some(ContentKind::Password) => Some(Kind::Password),
                Some(ContentKind::Text) => Some(Kind::Text),
                Some(ContentKind::Other) => Some(Kind::Other),
                _ => None,
            }
        }

        if preview.is_none() {
            preview = match content_kind {
                Some(ContentKind::Text | ContentKind::File | ContentKind::Html | ContentKind::Markdown | ContentKind::Password) => match &t.data {
                    Data::Inline(buff) => Some(String::from_utf8_lossy(&buff).as_bytes().to_vec()),
                    Data::File(payload_file) => payload_file.head().map(|head| String::from_utf8_lossy(head).as_bytes().to_vec()),
                },
                _ => None,
            }
        }

        if thumb.is_none()
            && let Some(ContentKind::Image) = content_kind
        {
            thumb = match &t.meta {
                Some(TypeMeta::ThumbNail(Some(thumb))) => Some(Thumb {
                    data: Some(thumb.jpeg.to_vec()),
                    state: clipleaf_ipc::protocol::v1::models::ThumbState::Ready,
                    format: Some("image/jpeg".to_string()),
                    width: Some(thumb.width as u32),
                    height: Some(thumb.height as u32),
                    src_width: Some(thumb.src_width),
                    src_height: Some(thumb.src_height),
                    orientation: Some(thumb.orientation),
                    has_alpha: Some(thumb.has_alpha),
                }),
                Some(TypeMeta::ThumbNail(None)) => Some(Thumb {
                    state: clipleaf_ipc::protocol::v1::models::ThumbState::Unavailable,
                    format: None,
                    width: None,
                    height: None,
                    src_width: None,
                    src_height: None,
                    orientation: None,
                    has_alpha: None,
                    data: None,
                }),
                _ => None,
            }
        }

        types.push(TypeSummary {
            mime: t.mime.mime().to_string(),
            size: t.size,
        });
    }

    EntrySummary {
        file_count: files_count,
        file_op: None,
        id: item.entry.id,
        kind: kind.unwrap_or(Kind::Other),
        pinned: item.pinned,
        preview: preview.unwrap_or_default(),
        score: None,
        size: size,
        thumb: thumb,
        time_ms: item.time_ms,
        types: types,
    }
}
