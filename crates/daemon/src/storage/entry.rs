use std::{
    borrow::Cow,
    fmt::Debug,
    fs::{self},
    mem,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

use clipleaf_core::{Mime, MimeKind, log_error, mime_types::TEXT_PLAIN};
use xxhash_rust::xxh3::Xxh3;

use crate::storage::{MAX_INLINE_TYPE_BYTES, PayloadFile, PayloadWriter};

#[derive(Debug)]
pub struct Entry {
    id: u64,
    key: u128,
    types: Vec<TypeData>,
    dir: Arc<Path>,
    committed: AtomicBool,
    pub(super) pending_types: Vec<Option<TypeData>>,
}

#[derive(Debug)]
pub struct TypeData {
    pub(super) mime: Mime<Cow<'static, str>>,
    pub(super) size: u64,
    pub(super) hash: u128, /* XXH3-128 */
    pub(super) data: Data,
}

impl TypeData {
    pub fn mime(&self) -> &Mime<Cow<'static, str>> {
        &self.mime
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

pub enum Data {
    Inline(Arc<[u8]>),
    File(PayloadFile), // payloads/<id>.<n>; head: first 5 KiB, text and file lists only
}

impl Debug for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inline(h) => f.debug_tuple("Inline").field(&h.len()).finish(),
            Self::File(p) => f.debug_tuple("File").field(p).finish(),
        }
    }
}

pub struct EntryBuilder {
    pub(super) entry: Entry,
}

impl EntryBuilder {
    pub fn new(id: u64, dir: &Path) -> Self {
        EntryBuilder {
            entry: Entry {
                id,
                key: 0,
                types: Vec::new(),
                dir: Arc::from(dir),
                committed: AtomicBool::new(false),
                pending_types: Vec::new(),
            },
        }
    }

    pub fn payload(&mut self, mime: Mime<Cow<'static, str>>) -> PayloadWriter {
        self.entry.pending_types.push(None);
        let seqnum = self.entry.pending_types.len() - 1;

        PayloadWriter {
            file: None,
            hasher: Xxh3::new(),
            head: Some(Vec::with_capacity(MAX_INLINE_TYPE_BYTES)),
            file_id: self.entry.id,
            file_seqnum: seqnum,
            file_path_base: self.entry.dir.clone(),
            mime: Some(mime),
            size: 0,
        }
    }

    pub fn create(mut self) -> Entry {
        let mut hasher = Xxh3::new();
        self.entry.types = mem::take(&mut self.entry.pending_types)
            .into_iter()
            .flatten()
            .filter(|t| t.size > 0)
            .collect();
        let mut sorted: Vec<&TypeData> = self.entry.types.iter().by_ref().collect();
        sorted.sort_unstable_by(|a, b| {
            a.mime
                .cmp_with(&b.mime)
                .then(a.size.cmp(&b.size))
                .then(a.hash.cmp(&b.hash))
                .then_with(|| get_mime_hash_bytes(&a.mime).cmp(get_mime_hash_bytes(&b.mime)))
        });
        for t in sorted {
            hasher.update(get_mime_hash_bytes(t.mime()));
            hasher.update(&[0u8]);
            hasher.update(&t.size.to_le_bytes());
            hasher.update(&t.hash.to_le_bytes())
        }

        self.entry.key = hasher.digest128();
        self.entry
    }
}

fn get_mime_hash_bytes<'a>(mime: &'a Mime<Cow<'static, str>>) -> &'a [u8] {
    let kind = mime.kind();
    if kind == MimeKind::Text || kind == MimeKind::TextExtended || kind == MimeKind::TextLike {
        return TEXT_PLAIN.as_bytes();
    }
    mime.mime().as_bytes()
}

impl Entry {
    pub fn commit(&self) {
        self.committed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn uncommit(&self) {
        self.committed
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn types(&self) -> &[TypeData] {
        &self.types
    }

    pub fn key(&self) -> u128 {
        self.key
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        if !self.committed.load(std::sync::atomic::Ordering::Relaxed) {
            for t in self.types.iter().chain(self.pending_types.iter().flatten()) {
                if let Data::File(f) = &t.data
                    && let Err(e) = fs::remove_file(&f.path)
                {
                    log_error!("Could not delete file on etry drop: {}", e);
                }
            }
        }
    }
}
