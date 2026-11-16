use std::io;

use clipleaf_core::{
    ContentKind, mime_types::{GNOME_COPIED_FILES, KDE_CUT_MARKER},
};
use clipleaf_ipc::protocol::{
    handshake::{ErrorCode, ErrorReply, Hello, HelloReply},
    v1::models::{EntrySummary, FileOp, Kind, List, ListReply, Thumb, TypeSummary},
};

use crate::{
    index::{Item, SnapshotHandle},
    ipc::message_handler::{ReplyWaitHandle, RequestMessageHandler},
    storage::{Data, TypeMeta},
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
            .map(|item| build_summary(item, msg.thumbs));

        reply.set_reply(Ok(ListReply {
            entries: entries.collect(),
            total: snapshot.items.len() as u64,
        }))
    }
}

fn build_summary(item: &Item, include_thumb: bool) -> EntrySummary {
    let mut types = Vec::with_capacity(item.entry.types.len());
    let mut file_op = None;
    for t in &item.entry.types {
        let files_count = match t.meta {
            Some(TypeMeta::FileList { items }) => {
                file_op.get_or_insert(FileOp::Copy);
                Some(items)
            }
            _ => None,
        };

        let content_kind = ContentKind::from_mime(&t.mime);
        let preview = match content_kind {
            Some(ContentKind::Text | ContentKind::File | ContentKind::Html | ContentKind::Markdown | ContentKind::Password) => match &t.data {
                Data::Inline(buff) => buff.to_vec(),
                Data::File(payload_file) => payload_file.head().map_or_default(|head| head.to_vec()),
            },
            _ => Vec::default(),
        };

        if t.mime.mime() == KDE_CUT_MARKER {
            file_op = Some(match &t.data {
                Data::Inline(b) if b.as_ref() == b"1" => FileOp::Cut,
                _ => FileOp::Copy,
            });
        } else if t.mime.mime() == GNOME_COPIED_FILES && preview.split(|&b| b == b'\n').next() == Some(b"cut") {
            file_op = Some(FileOp::Cut);
        }

        let kind = match content_kind {
            Some(ContentKind::File) => Kind::File,
            Some(ContentKind::Html) => Kind::Html,
            Some(ContentKind::Image) => Kind::Image,
            Some(ContentKind::Markdown) => Kind::Markdown,
            Some(ContentKind::Password) => Kind::Password,
            Some(ContentKind::Text) => Kind::Text,
            Some(ContentKind::Other) => Kind::Other,
            _ => continue,
        };

        let thumb = match (content_kind, &t.meta) {
            (Some(ContentKind::Image), Some(TypeMeta::ThumbNail(Some(thumb)))) => Some(Thumb {
                data: include_thumb.then(|| thumb.jpeg.to_vec()),
                state: clipleaf_ipc::protocol::v1::models::ThumbState::Ready,
                format: Some("image/jpeg".to_string()),
                width: Some(thumb.width as u32),
                height: Some(thumb.height as u32),
                src_width: Some(thumb.src_width),
                src_height: Some(thumb.src_height),
                orientation: Some(thumb.orientation),
                has_alpha: Some(thumb.has_alpha),
            }),
            (Some(ContentKind::Image), Some(TypeMeta::ThumbNail(None))) => Some(Thumb {
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
            (Some(ContentKind::Image), None) => Some(Thumb {
                state: clipleaf_ipc::protocol::v1::models::ThumbState::Pending,
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
        };

        types.push(TypeSummary {
            mime: t.mime.mime().to_string(),
            size: t.size,
            file_count: files_count,
            kind,
            preview,
            thumb,
        });
    }

    EntrySummary {
        id: item.entry.id,
        pinned: item.pinned,
        score: None,
        time_ms: item.time_ms,
        types,
        file_op,
    }
}
