use std::{borrow::Cow, sync::Arc};

use clipleaf_core::{classify_mime, log_debug, log_error};
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

use crate::{
    index::Item,
    storage::{Data, MAX_FETCHED_TYPES, PayloadFile, TypeData, wal::error::WalError},
};

pub const IDENTIFIER: [u8; 14] = *b"CLIPLEAF WAL\0\0";
pub const VERSION: u16 = 1;

// identifier + version + salt + seq_snapshot + checksum;
pub const HEADER_LEN: usize = size_of_val(&IDENTIFIER) + size_of_val(&VERSION) + 8 + 8 + 8;

pub const MAX_RECORD_LEN: u32 = 256 * 1024;

pub struct Header {
    pub salt: u64,
    pub seq_snapshot: u64,
}

#[derive(Debug)]
pub enum RestoreRecord {
    Insert(RestoreItem),
    Delete { id: u64 },
}

#[derive(Debug)]
pub struct RestoreItem {
    pub id: u64,
    pub seq: u64,
    pub time_ms: u64,
    pub pinned: bool,
    pub types: Vec<TypeData>,
}

const INSERT_TYPE: u8 = 1;
const DELETE_TYPE: u8 = 2;

pub fn encode_header(h: &Header, buff: &mut [u8; HEADER_LEN]) {
    let mut offset = 0;

    encode_without_size_check(&mut offset, buff, &IDENTIFIER);
    encode_without_size_check(&mut offset, buff, &VERSION.to_le_bytes());
    encode_without_size_check(&mut offset, buff, &h.salt.to_le_bytes());
    encode_without_size_check(&mut offset, buff, &h.seq_snapshot.to_le_bytes());

    let header_payload = &buff[0..offset];
    let checksum = xxh3_64(header_payload);
    encode_without_size_check(&mut offset, buff, &checksum.to_le_bytes());
}

pub fn decode_header(buff: &[u8]) -> Result<Header, WalError> {
    if buff.len() < HEADER_LEN {
        return Err(WalError::Corrupted);
    }

    if buff[0..size_of_val(&IDENTIFIER)] != IDENTIFIER {
        return Err(WalError::BadIdentifier);
    }

    let computed_checksum = xxh3_64(&buff[0..HEADER_LEN - 8]);
    let (checksum, _) = read_u64(&buff[HEADER_LEN - 8..])?;
    if computed_checksum != checksum {
        return Err(WalError::Corrupted);
    }

    let (version, rest) = read_u16(&buff[size_of_val(&IDENTIFIER)..])?;
    if version != VERSION {
        return Err(WalError::UnsupportedVersion(version));
    }

    let (salt, rest) = read_u64(rest)?;
    let (seq_snapshot, _) = read_u64(rest)?;

    Ok(Header { salt, seq_snapshot })
}

/// encoded as len, type, id, seq, time, pinned, entry count, [entry], checksum
pub fn encode_append(item: &Item, salt: u64, buff: &mut [u8]) -> Result<usize, WalError> {
    let t: u8 = INSERT_TYPE;
    let mut offset = 4; // the len

    // type
    encode_with_size_check(&mut offset, buff, &t.to_le_bytes())?;
    log_debug!("encode: entry id: {}", item.entry.id);
    encode_with_size_check(&mut offset, buff, &item.entry.id.to_le_bytes())?;
    encode_with_size_check(&mut offset, buff, &item.seq.to_le_bytes())?;
    encode_with_size_check(&mut offset, buff, &item.time_ms.to_le_bytes())?;
    encode_with_size_check(&mut offset, buff, &[item.pinned as u8])?;
    encode_with_size_check(
        &mut offset,
        buff,
        &(item.entry.types.len() as u32).to_le_bytes(),
    )?;
    for typedata in item.entry.types.iter() {
        encode_stored_type_data(typedata, &mut offset, buff)?;
    }

    if offset as u32 > MAX_RECORD_LEN {
        return Err(WalError::Corrupted);
    }

    encode_len_and_checksum(offset as u32, salt, buff)?;

    Ok(offset + 8)
}

pub fn encode_delete(id: u64, salt: u64, buff: &mut [u8]) -> Result<usize, WalError> {
    let t: u8 = DELETE_TYPE;
    let mut offset = 4; // the len

    // type
    encode_with_size_check(&mut offset, buff, &t.to_le_bytes())?;
    encode_with_size_check(&mut offset, buff, &id.to_le_bytes())?;

    if offset as u32 > MAX_RECORD_LEN {
        return Err(WalError::Corrupted);
    }

    encode_len_and_checksum(offset as u32, salt, buff)?;
    Ok(offset + 8)
}

pub fn decode_record(buff: &[u8], salt: u64) -> Result<(usize, RestoreRecord), WalError> {
    let (len, _rest) = read_u32(buff)?;

    log_debug!("Checking len field.. {}, {}", len, buff.len());
    // len needs to be at least size(u32) + size(u8) for len + type
    if !(5..=MAX_RECORD_LEN).contains(&len) || buff.len() < 8 + len as usize {
        return Err(WalError::Corrupted);
    }

    log_debug!(
        "decode: Calculating checksum for {} bytes with salt {}",
        len,
        salt
    );
    let record_checksum = xxh3_64_with_seed(&buff[..len as usize], salt);
    log_debug!("Reading checksum at offset: {}", len);
    let (stored_checksum, _) = read_u64(&buff[len as usize..8 + len as usize])?;

    log_debug!(
        "decode: Record checksum: {}  stored checksum: {}",
        record_checksum,
        stored_checksum
    );
    if record_checksum != stored_checksum {
        log_error!("Record checksums do not match.");
        return Err(WalError::Corrupted);
    }

    let rest = &buff[4..len as usize];

    let (t, rest) = read_u8(rest)?;

    match t {
        INSERT_TYPE => {
            let (id, rest) = read_u64(rest)?;
            log_debug!("decode: entry id: {}", id);

            let (seq, rest) = read_u64(rest)?;
            let (time_ms, rest) = read_u64(rest)?;
            let (pinned, rest) = read_bool(rest)?;
            let (entry_count, rest) = read_u32(rest)?;

            if entry_count as usize > MAX_FETCHED_TYPES {
                return Err(WalError::Corrupted);
            }

            let mut types = Vec::with_capacity(entry_count as usize);
            let mut rest = rest;
            for _ in 0..entry_count {
                let (_, rest_types, entry) = decode_stored_type_data(rest, id)?;
                rest = rest_types;
                types.push(entry);
            }

            Ok((
                len as usize + 8,
                RestoreRecord::Insert(RestoreItem {
                    seq,
                    time_ms,
                    pinned,
                    id,
                    types,
                }),
            ))
        }
        DELETE_TYPE => {
            let (id, _) = read_u64(rest)?;

            Ok((len as usize + 8, RestoreRecord::Delete { id }))
        }
        t => Err(WalError::UnknownType((len as usize + 8, t))),
    }
}

fn encode_len_and_checksum(len: u32, salt: u64, buff: &mut [u8]) -> Result<(), WalError> {
    buff[0..4].copy_from_slice(&len.to_le_bytes());
    log_debug!(
        "encode: Calculating checksum for {} bytes with salt {}",
        len,
        salt
    );
    let checksum = &xxh3_64_with_seed(&buff[..len as usize], salt);
    log_debug!("encode: checksum: {} at offset {}", checksum, len);
    let mut ofset: usize = len as usize;
    encode_with_size_check(&mut ofset, buff, &checksum.to_le_bytes())?;
    Ok(())
}

fn decode_stored_type_data(buff: &[u8], id: u64) -> Result<(usize, &[u8], TypeData), WalError> {
    let (mime_len, rest) = read_u32(buff)?;
    let (mime, rest) = rest
        .split_at_checked(mime_len as usize)
        .ok_or(WalError::Corrupted)?;
    let mime = Cow::from(String::from_utf8(mime.to_vec()).map_err(|e| {
        log_error!("Failed to decode mime string: {e}");
        WalError::Corrupted
    })?);

    let (size, rest) = read_u64(rest)?;
    let (hash, rest) = read_u128(rest)?;
    let (has_file, rest) = read_bool(rest)?;
    let (file_seq, rest) = read_u32(rest)?;
    let (head_len, rest) = read_u32(rest)?;
    let (head, _) = rest
        .split_at_checked(head_len as usize)
        .ok_or(WalError::Corrupted)?;

    let data = match has_file {
        true => Data::File(PayloadFile::restored(id, file_seq as usize, head)),
        false => Data::Inline(Arc::from(head)),
    };

    let offset = 4 + mime_len + 8 + 16 + 1 + 4 + 4 + head_len;

    Ok((
        offset as usize,
        &buff[offset as usize..],
        TypeData {
            data,
            mime: classify_mime(mime),
            hash,
            size,
        },
    ))
}

/// encoded as len(mime), mime, size, hash, has_file, seq, head bytes len, head bytes
fn encode_stored_type_data(
    t: &TypeData,
    offset: &mut usize,
    buff: &mut [u8],
) -> Result<usize, WalError> {
    let original = *offset;
    log_debug!("mime len: {}", t.mime.mime().len());
    encode_with_size_check(offset, buff, &(t.mime.mime().len() as u32).to_le_bytes())?;
    encode_with_size_check(offset, buff, t.mime.mime().as_bytes())?;
    encode_with_size_check(offset, buff, &t.size.to_le_bytes())?;
    encode_with_size_check(offset, buff, &t.hash.to_le_bytes())?;

    let (has_file, file_seq, head) = match t.data {
        crate::storage::Data::Inline(ref head) => (0u8, 0_usize, Some(head.clone())),
        crate::storage::Data::File(ref payload_file) => {
            (1u8, payload_file.seqnum, payload_file.head.clone())
        }
    };

    encode_with_size_check(offset, buff, &[has_file])?;
    encode_with_size_check(offset, buff, &(file_seq as u32).to_le_bytes())?;
    encode_with_size_check(
        offset,
        buff,
        &head
            .as_ref()
            .map_or_default(|h| h.len() as u32)
            .to_le_bytes(),
    )?;

    if let Some(head_bytes) = head {
        encode_with_size_check(offset, buff, &head_bytes)?;
    }

    let len = *offset - original;

    Ok(len)
}

pub fn encode_without_size_check(offset: &mut usize, buff: &mut [u8], src: &[u8]) {
    buff[*offset..*offset + src.len()].copy_from_slice(src);
    *offset += src.len();
}

pub fn encode_with_size_check(
    offset: &mut usize,
    buff: &mut [u8],
    src: &[u8],
) -> Result<(), WalError> {
    let slice = &mut buff
        .get_mut(*offset..*offset + src.len())
        .ok_or(WalError::SmallEncodeBuffer)?;
    if slice.len() != src.len() {
        return Err(WalError::SmallEncodeBuffer);
    }

    slice.copy_from_slice(src);
    *offset += src.len();

    Ok(())
}

fn read_bool(buff: &[u8]) -> Result<(bool, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<1>().ok_or(WalError::Corrupted)?;
    let val = u8::from_le_bytes(*val_bytes);

    if val > 1 {
        return Err(WalError::Corrupted);
    }
    Ok((val == 1, rest))
}

fn read_u8(buff: &[u8]) -> Result<(u8, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<1>().ok_or(WalError::Corrupted)?;
    let val = u8::from_le_bytes(*val_bytes);

    Ok((val, rest))
}

fn read_u16(buff: &[u8]) -> Result<(u16, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<2>().ok_or(WalError::Corrupted)?;
    let val = u16::from_le_bytes(*val_bytes);

    Ok((val, rest))
}

fn read_u32(buff: &[u8]) -> Result<(u32, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<4>().ok_or(WalError::Corrupted)?;
    let val = u32::from_le_bytes(*val_bytes);

    Ok((val, rest))
}

fn read_u64(buff: &[u8]) -> Result<(u64, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<8>().ok_or(WalError::Corrupted)?;
    let val = u64::from_le_bytes(*val_bytes);

    Ok((val, rest))
}

fn read_u128(buff: &[u8]) -> Result<(u128, &[u8]), WalError> {
    let (val_bytes, rest) = buff.split_first_chunk::<16>().ok_or(WalError::Corrupted)?;
    let val = u128::from_le_bytes(*val_bytes);

    Ok((val, rest))
}
