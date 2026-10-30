# clipleaf IPC protocol, version 1

How clients talk to the history daemon `clipleafd`.
The reference implementation is the `clipleaf-ipc` crate: frame layout in
[`src/protocol/header.rs`](src/protocol/header.rs), message bodies in
[`src/protocol/v1/models.rs`](src/protocol/v1/models.rs) and
[`src/protocol/handshake.rs`](src/protocol/handshake.rs).

Status: **draft**. Version 1 is not frozen until the first release that ships `clipleafd`.

## 1. Transport

- A Unix stream socket at `$XDG_RUNTIME_DIR/clipleaf/daemon.sock`.
  If `XDG_RUNTIME_DIR` is unset, empty or relative, neither side uses a fallback path:
  both report an error.
- The daemon limits the number of open connections. A connection over the limit is accepted
  and closed at once, with no message: the client sees end-of-file or a broken pipe.
- One connection carries any number of requests, replies and events, in both directions,
  for as long as the client keeps it open.

## 2. Frames

Every message is one frame: a fixed 12-byte header followed by `len` bytes of body.
Header integers are little-endian. (Bodies are MessagePack, which has its own big-endian integer encoding.)

Notation used below: `name: T?` is optional (MessagePack `nil` or absent); `name: T = x` defaults to `x` when
absent (`nil` is NOT accepted for a defaulted field); anything else is required.

| Offset | Size | Field      | Meaning |
|-------:|-----:|------------|---------|
| 0      | 4    | `len`      | Body length in bytes, header excluded. |
| 4      | 1    | `category` | 1 request, 2 reply, 3 error, 4 event. |
| 5      | 1    | `index`    | Which request or event (tables below). |
| 6      | 1    | `nfds`     | Reserved hint, sent as 0 (see below). |
| 7      | 1    | `flags`    | Reserved hint, sent as 0 (see below). |
| 8      | 4    | `req_id`   | Request id; 0 for events and connection errors. |

### Size caps

| Frame | Max body + header |
|-------|-------------------|
| Any frame a client sends (the daemon's reader) | 64 KiB |
| Any frame the daemon sends (the client's reader) | 32 MiB |

A reader checks `len` against its cap **before** allocating. A frame over the cap is not
skipped: the peer is broken or hostile, so the reader closes the connection. The largest legitimate frame is a
`List` reply of 100 entries with thumbnails, about 17 MiB in the worst case.

### Reserved fields

`nfds` and `flags` are sent as 0 in version 1, and receivers ignore their values. Any future
use may only add an optional hint that an older peer can ignore. Anything that changes how
frames are read (for example passing file descriptors) must first be agreed on through
`Hello.features`. A receiver closes any file descriptors that arrive unannounced.

## 3. Message types

The pair `(category, index)` identifies the message. A reply has the same `index` as its request.

| Request    | index | Request frame | Reply frame | Notes |
|------------|------:|---------------|-------------|-------|
| `Hello`    | 1 | `01 01` | `02 01` | Must be the first frame. |
| `Status`   | 2 | `01 02` | `02 02` | |
| `List`     | 3 | `01 03` | `02 03` | |
| `Get`      | 4 | `01 04` | `02 04` | |
| `Restore`  | 5 | `01 05` | `02 05` | |
| `Pin`      | 6 | `01 06` | `02 06` | |
| `Delete`   | 7 | `01 07` | `02 07` | |
| `Clear`    | 8 | `01 08` | `02 08` | |
| `Subscribe`| 9 | `01 09` | `02 09` | |
| reserved   | 10 | `01 0A` | `02 0A` | Planned: `Paste`. |

| Event              | index | Frame |
|--------------------|------:|-------|
| `Added`            | 1 | `04 01` |
| `Updated`          | 2 | `04 02` |
| `Removed`          | 3 | `04 03` |
| `Cleared`          | 4 | `04 04` |
| `SelectionChanged` | 5 | `04 05` |

`Error` is `03 00`; category 3 has no other index.

### Unknown types

- **Daemon** receiving a request (category 1) with an unknown index: it replies `Error`
  `unknown_request` with the same `req_id` and keeps the connection open. The body was
  read in full, so the stream stays in sync.
- **Daemon** receiving any other category from a client (a reply, an error, an event or an
  unknown category): `Error` `bad_request` with `req_id` 0, then it closes the connection.
- **Client** receiving an unknown event index, or an unknown category with `req_id` 0:
  it skips the frame. A newer daemon may send event types an older client doesn't know.
- **Client** receiving a frame that carries a pending request's `req_id` but is neither that
  request's reply type nor `Error` (including an unknown type): that request fails; the
  connection stays usable.
- **Client** receiving a reply or error whose `req_id` matches no pending request (a late
  reply after a timeout): it drops the frame.

## 4. Bodies

Every body is a MessagePack **map with field names as keys** (rmp-serde `write_named`).
Each message type is one plain struct. The body carries no type tag, because the header
already says what it is.

Rules, which keep old and new peers compatible:

- **Unknown fields are ignored.** A newer peer may add fields freely.
- **Version 1 fields are required** unless this document marks them optional (`T?`) or
  gives them a default (`= x`). Fields added after version 1 are always optional or defaulted.
- **Integer ranges are fixed:** ids, sizes, offsets, times and totals are `u64`; `limit` and
  `file_count` are `u32`; `orientation` is `u8` (see each field in §6). On the wire any
  MessagePack integer format may carry the value (encoders write the smallest one).
  Ids stay at or below 2^53 − 1, so JSON-based and JavaScript clients can hold them exactly.
- **Byte fields** (`preview`, thumbnail `data`, `Get` `data`) are MessagePack `bin`, not arrays.
- **Empty messages** are an empty map `{}` (byte `0x80`), so a later version can add fields.
- **Enum-like fields** are snake_case strings. Where an unknown value has a safe meaning, a
  receiver maps it to a fallback:

| Field | Values | Unknown value → |
|-------|--------|-----------------|
| `kind` | `text`, `html`, `markdown`, `image`, `file`, `other` | `other` |
| `file_op` | `copy`, `cut` | `copy` |
| `thumb.state` | `ready`, `pending`, `unavailable` | `unavailable` |
| `code` (errors) | see §8 | `generic` |
| `selection` (requests) | `clipboard`, `primary`, `both` | **no fallback**: `bad_request` |
| `selection` (`SelectionChanged`) | `clipboard`, `primary` | `other`: the client ignores the event |
| `formats` | `all`, `text`, `mime` | **no fallback**: `bad_request` |

In requests, `selection` and `formats` choose an action. Guessing on an unknown value would do something
the client didn't ask for, so the request is rejected instead.

## 5. Session

### Handshake

The first frame a client sends must be `Hello`. The daemon answers anything else with
`Error` `bad_request` (`req_id` 0) and closes the connection. A second `Hello` later on the
same connection gets `Error` `bad_request` with that frame's `req_id`; the connection stays open.

```
Hello      { versions: [u32], features: [string] = [] }
HelloReply { version: u32, features: [string] = [], daemon: string? }
```

- `versions` lists every protocol version the client speaks. The daemon picks one of them
  that it also speaks and returns it in `version`; a client must accept any version it
  listed. If there is none, the daemon replies `Error` `unsupported_version` and closes the
  connection.
- `features` lists optional extensions. The reply lists the ones both sides will use: those
  the client offered that the daemon also supports.
  Version 1 defines none.
- `daemon` is a free-form string such as `"clipleafd 0.1.0"`, for diagnostics only.

### Request ids

- The client picks a non-zero `req_id` for each request. The reference client counts up from
  1, wraps around and skips 0.
- The daemon copies the request's `req_id` into its `Reply` or `Error`.
- A client may send several requests before reading replies. Replies may arrive **in a
  different order** from the requests (a slow search must not hold up a `Status`).
- **Read your writes:** the reply to a request that changes the history (`Restore`, `Pin`,
  `Delete`, `Clear`) is sent only after the change is visible to `List`. A client that gets
  the reply and then sends `List` sees the change.
- A client that stopped waiting for a reply (a timeout) drops it when it arrives later.
  The reply's `req_id` won't match any pending request.

### Errors

```
Error { code: string, message: string = "" }
```

- With the `req_id` of a request: that request failed. The connection stays usable.
- With `req_id` 0: the whole connection failed (for example a
  missing `Hello`). The daemon closes the connection after sending it.
- `message` is human-readable text for logs, not for parsing.

## 6. Requests

Notation as in §2: `T?` optional, `= x` default when absent.

### `Status`
```
Status      {}
StatusReply { entries: u64, pinned: u64, history_limit: u64,
              clipboard_id: u64?, primary_id: u64? }
```
`clipboard_id` / `primary_id`: the history entry that selection holds now; `nil` when it is
empty or its content wasn't recorded.

### `List`
```
List      { query: string?, offset: u64 = 0, limit: u32?, thumbs: bool = false }
ListReply { entries: [EntrySummary], total: u64 }
```
- Entries are newest first.
- `limit` is at most 100. The daemon lowers larger values to 100; `nil` means 100.
- `total` counts every entry that matches, so a client pages with `offset += entries.len()`
  until `offset >= total` or a page comes back empty.
- Offset paging is not a consistent snapshot: if the history changes between pages, an
  entry can be skipped or repeated. For a consistent view, `Subscribe` first and apply events
  on top of the pages.
- `query`: case-insensitive substring search over the entries' content. Results carry a
  `score`. With `nil`, the daemon lists everything.
- `thumbs`: include thumbnail image bytes (`thumb.data`). Without it, `thumb` still
  carries the state and dimensions.

### `Get`
```
Get      { id: u64, mime: string? }
GetReply { id: u64, mime: string, size: u64, data: bin? }
```
- `mime` `nil` means the entry's first (preferred) type.
- Types up to 5 KiB come back inline in `data`. **Open:** how larger, file-backed types are
  delivered is not decided yet.
- Unknown `id` or `mime`: `Error` `not_found`.

### `Restore`
```
Restore      { id: u64, selection: "clipboard"|"primary"|"both",
               formats: "all"|"text"|"mime", mime: string? }
RestoreReply {}
```
Makes the daemon offer entry `id` as the selection again. `formats` picks what to offer:
every stored type, text types only, or the one type named in `mime` (required when
`formats` is `mime`). The entry moves to the top of the history.

### `Pin`
```
Pin      { id: u64, pinned: bool }
PinReply {}
```
Pinned entries are never evicted by the history limit and survive `Clear` by default.

### `Delete`
```
Delete      { id: u64 }
DeleteReply {}
```

### `Clear`
```
Clear      { include_pinned: bool = false }
ClearReply { removed: u64 }
```

### `Subscribe`
```
Subscribe      { thumbs: bool = false }
SubscribeReply {}
```
Starts the event stream on this connection (§7). `thumbs` works as it does in `List`.

### `EntrySummary`
```
EntrySummary {
  id: u64, kind: string, time_ms: u64, pinned: bool,
  size: u64,                 // all stored types together, in bytes
  preview: bin,              // up to 256 characters from the start; UTF-8 for text kinds
  types: [{ mime: string, size: u64 }],
  thumb: Thumb?,             // images only
  file_count: u32?,          // file lists only
  file_op: string?,          // file lists only: "copy" | "cut"
  score: f32?                // search results only
}

Thumb {
  state: "ready"|"pending"|"unavailable",
  format: string?,           // media type of data, e.g. "image/jpeg"
  width: u32?, height: u32?,
  src_width: u32?, src_height: u32?,   // original image
  orientation: u8?,          // EXIF orientation (1–8), already applied
  has_alpha: bool?,          // original has transparency; the thumbnail is flattened
  data: bin?                 // only when requested (List.thumbs / Subscribe.thumbs)
}
```
`time_ms` is milliseconds since the Unix epoch, taken when the daemon recorded the copy.

## 7. Events

Events are sent only after the `SubscribeReply` (never before it on the connection). Their
`req_id` is 0.

```
Added            { entry: EntrySummary }
Updated          { entry: EntrySummary }
Removed          { id: u64 }
Cleared          {}
SelectionChanged { selection: "clipboard"|"primary", id: u64? }
```

How a client applies them:

- `Added` and `Updated` are **upserts by id**: insert the entry, or replace it if present.
- `Removed` removes the entry if present.
- `Cleared` means **the client's view is stale as a whole: re-`List` it**. The daemon sends
  it after a `Clear` (from any client), and may send it after any other change that touches
  many entries at once. It carries no data on purpose: the client doesn't work out what
  survived (pinned entries, for example); `List` tells it. Events that arrive after
  `Cleared` are applied on top of the new `List` result, as usual. A client handles it the
  same way as falling behind (below).
- `SelectionChanged`: the selection now holds entry `id`, or `nil` when it is empty or
  unrecorded. It is sent when a new copy has been recorded, and right away on a clear or a
  restore.

Guarantees from the daemon:

- **Ordered:** events and replies on one connection arrive in the order the daemon sent them.
- **Visible first:** an event is sent only after its change is visible to `List`.
- **Coalesced:** the daemon sends the difference between what the client last saw and the
  current state, not every intermediate step. A client always converges to the current
  state, but it may never see some short-lived entries.

Together these make the usual start-up race-free: send `Subscribe`, then `List`, and apply
events on top of the `List` result. An event that the `List` already reflects is applied
again, which does nothing because every event is an upsert or a remove.

### Falling behind

If a client stops reading, the daemon doesn't keep an unbounded queue for it. The reference
client library caps its own queue at 1024 events. When the cap is hit, it drops the queued
events and every new one, then reports the gap once (`ClientError::EventQueueFull`). The
client then sends `List` again and continues from that snapshot.

## 8. Error codes

| Code | Meaning |
|------|---------|
| `unknown_request` | The request index is not known to this daemon. |
| `bad_request` | The body didn't decode, a field is out of range, or a frame was out of order (no `Hello` first, a non-request from the client). |
| `not_found` | No entry (or no type) with that id or mime. |
| `unsupported_version` | No common protocol version (`req_id` 0; connection closed). |
| `internal` | The daemon failed (I/O, storage). The request may be retried. |
| `cancelled` | A newer request replaced this one (for example a newer search). |
| `generic` | Fallback for a code this client doesn't know. |
