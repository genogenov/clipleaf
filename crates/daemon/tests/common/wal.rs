//! The `wal.log` format as the tests read and write it. It is a versioned on-disk contract (an
//! older or newer build reads the same file), so the tests may depend on it, unlike log lines.
//!
//! Header (40 bytes): identifier (14) | version u16 | salt u64 | seq_snapshot u64 | xxh3_64 of
//! the first 32 bytes. Record: len u32 (covers len + type + body) | type u8 | body |
//! xxh3_64_with_seed(first len bytes, salt). All little-endian.

use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

pub const IDENTIFIER: &[u8; 14] = b"CLIPLEAF WAL\0\0";
pub const HEADER_LEN: usize = 40;
pub const INSERT: u8 = 1;
pub const DELETE: u8 = 2;
pub const TOUCH: u8 = 3;

pub fn salt_of(wal: &[u8]) -> u64 {
    u64::from_le_bytes(wal[16..24].try_into().unwrap())
}

pub fn header(version: u16, salt: u64) -> Vec<u8> {
    let mut h = IDENTIFIER.to_vec();
    h.extend(version.to_le_bytes());
    h.extend(salt.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(xxh3_64(&h).to_le_bytes());
    h
}

/// A record with a valid checksum.
pub fn record(salt: u64, kind: u8, body: &[u8]) -> Vec<u8> {
    let len = u32::try_from(4 + 1 + body.len()).unwrap();
    let mut r = len.to_le_bytes().to_vec();
    r.push(kind);
    r.extend_from_slice(body);
    let sum = xxh3_64_with_seed(&r, salt);
    r.extend(sum.to_le_bytes());
    r
}

#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Insert(Insert),
    Delete { id: u64 },
    Touch { id: u64, seq: u64 },
    Unknown(u8),
}

/// An Insert record: one history entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub id: u64,
    pub seq: u64,
    pub types: Vec<Stored>,
}

/// One fetched type of an entry: inline types keep all their bytes in `head`, types in a
/// payload file keep only a text head (or nothing).
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub mime: String,
    pub size: u64,
    pub in_file: bool,
    pub head: Vec<u8>,
}

impl Insert {
    /// `(mime, size)` of each type, in the order they are stored.
    pub fn sizes(&self) -> Vec<(&str, u64)> {
        self.types
            .iter()
            .map(|t| (t.mime.as_str(), t.size))
            .collect()
    }
}

/// Every whole record after a valid header, up to the first one that doesn't decode: a record
/// still being written, or damage. A file without a valid header has none.
pub fn records(wal: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    if wal.len() < HEADER_LEN || &wal[..14] != IDENTIFIER {
        return out;
    }
    let salt = salt_of(wal);
    let mut rest = &wal[HEADER_LEN..];
    while let Some((record, next)) = next_record(rest, salt) {
        out.push(record);
        rest = next;
    }
    out
}

fn next_record(buf: &[u8], salt: u64) -> Option<(Record, &[u8])> {
    let len = u32::from_le_bytes(buf.get(..4)?.try_into().unwrap()) as usize;
    if len < 5 || buf.len() < len + 8 {
        return None;
    }
    let sum = u64::from_le_bytes(buf[len..len + 8].try_into().unwrap());
    if xxh3_64_with_seed(&buf[..len], salt) != sum {
        return None;
    }
    let mut body = Reader(&buf[5..len]);
    let record = match buf[4] {
        INSERT => {
            let id = body.u64()?;
            let seq = body.u64()?;
            let _time_ms = body.u64()?;
            let _pinned = body.take(1)?;
            let count = body.u32()?;
            let mut types = Vec::new();
            for _ in 0..count {
                let mime_len = body.u32()? as usize;
                let mime = String::from_utf8(body.take(mime_len)?.to_vec()).ok()?;
                let size = body.u64()?;
                let _hash = body.take(16)?;
                let in_file = body.take(1)?[0] != 0;
                let _file_seq = body.u32()?;
                let head_len = body.u32()? as usize;
                let head = body.take(head_len)?.to_vec();
                types.push(Stored {
                    mime,
                    size,
                    in_file,
                    head,
                });
            }
            Record::Insert(Insert { id, seq, types })
        }
        DELETE => Record::Delete { id: body.u64()? },
        TOUCH => Record::Touch {
            id: body.u64()?,
            seq: body.u64()?,
        },
        kind => Record::Unknown(kind),
    };
    Some((record, &buf[len + 8..]))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}
