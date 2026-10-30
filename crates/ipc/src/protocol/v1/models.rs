//! Wire types: one plain struct per message, MessagePack named maps (rmp-serde `to_vec_named`).
//!
//! Forward compatibility rules (decisions 2026-10-26/27):
//! - Unknown fields are ignored on decode (never `deny_unknown_fields`).
//! - v1 fields are required unless marked `#[serde(default)]` or `Option`; fields added later
//!   are always `Option` or `#[serde(default)]`.
//! - Enum-like fields are snake_case strings. Fields with a safe meaning for an unknown value have
//!   a `#[serde(other)]` fallback; `Selection` and `Formats` have none (an unknown value is a
//!   decode error → `bad_request`, never a different action). Events use `EventSelection`, which
//!   has one (a client skips a `SelectionChanged` for a selection it doesn't know).
//! - Integers are fixed width (u64 ids/sizes/offsets/times, u32 counts), never usize.
//! - Byte fields use `serde_bytes` (msgpack bin, not an int array). An `Option` byte field also
//!   needs `default`: with `with = "serde_bytes"` a missing field is otherwise an error.
//! - Empty messages are `{}` structs, never unit structs: `{}` encodes as an empty map and still
//!   decodes when a later version adds fields; a unit struct encodes as an empty array and rejects
//!   a map.

use serde::{Deserialize, Serialize};

use crate::protocol::bounded_vec;
use crate::protocol::header::{EventType, MessageType, RequestType};

// ---- Enum-like fields ----

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    Html,
    Markdown,
    Image,
    File,
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileOp {
    Cut,
    #[serde(other)]
    Copy,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThumbState {
    Ready,
    Pending,
    #[serde(other)]
    Unavailable,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    Clipboard,
    Primary,
    Both,
}

/// The selection a `SelectionChanged` event is about. Unlike `Selection` (a request's target),
/// an unknown value from a newer daemon decodes as `Other`, which a client ignores.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSelection {
    Clipboard,
    Primary,
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Formats {
    All,
    Text,
    Mime,
}

// ---- Nested types ----

/// One history entry as clients see it (List results and events).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct EntrySummary {
    pub id: u64,
    pub kind: Kind,
    pub time_ms: u64,
    pub pinned: bool,
    /// Total size of all stored types in bytes.
    pub size: u64,
    /// At most 256 characters cut from the head (UTF-8 for text kinds; may end mid-line).
    #[serde(with = "serde_bytes")]
    pub preview: Vec<u8>,
    #[serde(deserialize_with = "bounded_vec::bounded_vec::<_,_,256>")]
    pub types: Vec<TypeSummary>,
    pub thumb: Option<Thumb>,
    /// Number of files, for file-list entries.
    pub file_count: Option<u32>,
    /// Copy or cut, for file-list entries.
    pub file_op: Option<FileOp>,
    /// Search relevance; set only on search results.
    pub score: Option<f32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TypeSummary {
    pub mime: String,
    pub size: u64,
}

/// Thumbnail state and metadata; the JPEG bytes only when the client asked for them
/// (`List::thumbs`, `Subscribe::thumbs`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub state: ThumbState,
    /// Media type of `data`, e.g. "image/jpeg".
    pub format: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Size of the original image.
    pub src_width: Option<u32>,
    pub src_height: Option<u32>,
    /// EXIF orientation already applied to the thumbnail (1-8).
    pub orientation: Option<u8>,
    /// The original has transparency (the thumbnail is flattened on a checkerboard).
    pub has_alpha: Option<bool>,
    #[serde(default, with = "serde_bytes")]
    pub data: Option<Vec<u8>>,
}

// ---- Requests and replies ----

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Status {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StatusReply {
    pub entries: u64,
    pub pinned: u64,
    pub history_limit: u64,
    /// History entry the clipboard holds now; None when cleared or not recorded.
    pub clipboard_id: Option<u64>,
    pub primary_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct List {
    /// Case-insensitive substring search (4f); None lists everything.
    pub query: Option<String>,
    #[serde(default)]
    pub offset: u64,
    /// At most 100; the daemon clamps larger values. None = 100.
    pub limit: Option<u32>,
    /// Include thumbnail JPEG bytes.
    #[serde(default)]
    pub thumbs: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ListReply {
    #[serde(deserialize_with = "bounded_vec::bounded_vec::<_,_,1000>")]
    pub entries: Vec<EntrySummary>,
    /// Entries matching the request in total (for paging).
    pub total: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Get {
    pub id: u64,
    /// None = the entry's first (preferred) type.
    pub mime: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GetReply {
    pub id: u64,
    pub mime: String,
    pub size: u64,
    /// Inline content (types ≤ 5 KiB). File-backed delivery is decided at 4e.
    #[serde(default, with = "serde_bytes")]
    pub data: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Restore {
    pub id: u64,
    pub selection: Selection,
    pub formats: Formats,
    /// Required when `formats` is `Mime`.
    pub mime: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RestoreReply {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub id: u64,
    pub pinned: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PinReply {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Delete {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DeleteReply {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Clear {
    /// Also remove pinned entries. The default keeps them.
    #[serde(default)]
    pub include_pinned: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ClearReply {
    pub removed: u64,
}

/// Start receiving events on this connection.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Subscribe {
    /// Include thumbnail JPEG bytes in events.
    #[serde(default)]
    pub thumbs: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SubscribeReply {}

// ---- Events (req_id 0, only after Subscribe) ----
// Clients treat them as idempotent: Added/Updated = upsert by id, Removed = remove if present.

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Added {
    pub entry: EntrySummary,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Updated {
    pub entry: EntrySummary,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Cleared {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SelectionChanged {
    pub selection: EventSelection,
    /// The history entry the selection holds now; None when cleared or not recorded.
    pub id: Option<u64>,
}

// ---- Message types ----

message! {
    Status => MessageType::Request(RequestType::Status);
    StatusReply => MessageType::Reply(RequestType::Status);
    List => MessageType::Request(RequestType::List);
    ListReply => MessageType::Reply(RequestType::List);
    Get => MessageType::Request(RequestType::Get);
    GetReply => MessageType::Reply(RequestType::Get);
    Restore => MessageType::Request(RequestType::Restore);
    RestoreReply => MessageType::Reply(RequestType::Restore);
    Pin => MessageType::Request(RequestType::Pin);
    PinReply => MessageType::Reply(RequestType::Pin);
    Delete => MessageType::Request(RequestType::Delete);
    DeleteReply => MessageType::Reply(RequestType::Delete);
    Clear => MessageType::Request(RequestType::Clear);
    ClearReply => MessageType::Reply(RequestType::Clear);
    Subscribe => MessageType::Request(RequestType::Subscribe);
    SubscribeReply => MessageType::Reply(RequestType::Subscribe);
    Added => MessageType::Event(EventType::Added);
    Updated => MessageType::Event(EventType::Updated);
    Removed => MessageType::Event(EventType::Removed);
    Cleared => MessageType::Event(EventType::Cleared);
    SelectionChanged => MessageType::Event(EventType::SelectionChanged);
}

request! {
    Status => StatusReply;
    List => ListReply;
    Get => GetReply;
    Restore => RestoreReply;
    Pin => PinReply;
    Delete => DeleteReply;
    Clear => ClearReply;
    Subscribe => SubscribeReply;
}
