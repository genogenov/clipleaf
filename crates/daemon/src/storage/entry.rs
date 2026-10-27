use std::{
    borrow::Cow,
    fmt::Debug,
    fs::{self},
    mem,
    path::Path,
    sync::Arc,
};

use clipleaf_core::{FileListFormat, FileListScan, Mime, MimeKind, log_error, mime_types::TEXT_PLAIN};
use xxhash_rust::xxh3::Xxh3;

use crate::storage::{MAX_INLINE_TYPE_BYTES, PayloadFile, PayloadWriter};

#[derive(Debug)]
pub struct Entry {
    pub id: u64,
    pub key: u128,
    pub payloads_dir: Arc<Path>,
    pub types: Vec<TypeData>,
    committed: bool,
    pub pending_types: Vec<Option<TypeData>>,
}

#[derive(Debug, Clone)]
pub struct TypeData {
    pub mime: Mime<Cow<'static, str>>,
    pub size: u64,
    pub hash: u128,
    pub data: Data,
    pub meta: Option<TypeMeta>,
}

pub struct ThumbNail {
    pub width: u16,
    pub height: u16,
    pub has_alpha: bool,
    pub src_width: u32,
    pub src_height: u32,
    pub orientation: u8,
    pub jpeg: Box<[u8]>,
}

impl Debug for ThumbNail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbNail")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("has_alpha", &self.has_alpha)
            .field("src_width", &self.src_width)
            .field("src_height", &self.src_height)
            .field("orientation", &self.orientation)
            .field("jpeg len", &self.jpeg.len())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub enum TypeMeta {
    FileList { items: u32 },
    ThumbNail(Option<Arc<ThumbNail>>),
}

#[derive(Clone)]
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
    pub fn new(id: u64, dir: Arc<Path>) -> Self {
        EntryBuilder {
            entry: Entry {
                id,
                key: 0,
                types: Vec::new(),
                committed: false,
                pending_types: Vec::new(),
                payloads_dir: dir,
            },
        }
    }

    pub fn payload(&mut self, mime: Mime<Cow<'static, str>>) -> PayloadWriter {
        self.entry.pending_types.push(None);
        let seqnum = self.entry.pending_types.len() - 1;
        let scanner = FileListFormat::from_mime(mime.mime()).map(FileListScan::new);
        PayloadWriter {
            file: None,
            hasher: Xxh3::new(),
            head: Some(Vec::with_capacity(MAX_INLINE_TYPE_BYTES)),
            entry_id: self.entry.id,
            file_seqnum: seqnum,
            file_path_base: self.entry.payloads_dir.clone(),
            mime: Some(mime),
            size: 0,
            filescan: scanner,
        }
    }

    pub fn create(mut self) -> Entry {
        self.entry.types = mem::take(&mut self.entry.pending_types)
            .into_iter()
            .flatten()
            .filter(|t| t.size > 0)
            .collect();

        self.entry.key = Entry::compute_key(&self.entry.types);
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
    pub fn restored(id: u64, types: Vec<TypeData>, payloads_dir: Arc<Path>) -> Self {
        Self {
            id,
            key: Entry::compute_key(&types),
            types,
            committed: true,
            pending_types: Vec::default(),
            payloads_dir,
        }
    }

    pub fn commit(&mut self) {
        self.committed = true;
    }

    pub fn drop_files(&self) {
        for t in self.types.iter().chain(self.pending_types.iter().flatten()) {
            if let Data::File(f) = &t.data
                && let Err(e) = fs::remove_file(f.payload_path(&self.payloads_dir, self.id))
            {
                log_error!("Could not delete file on etry drop: {}", e);
            }
        }
    }

    pub fn compute_key(types: &[TypeData]) -> u128 {
        let mut hasher = Xxh3::new();
        let mut sorted: Vec<&TypeData> = types.iter().by_ref().collect();
        sorted.sort_unstable_by(|a, b| {
            a.mime
                .cmp_with(&b.mime)
                .then(a.size.cmp(&b.size))
                .then(a.hash.cmp(&b.hash))
                .then_with(|| get_mime_hash_bytes(&a.mime).cmp(get_mime_hash_bytes(&b.mime)))
        });
        for t in sorted {
            hasher.update(get_mime_hash_bytes(&t.mime));
            hasher.update(&[0u8]);
            hasher.update(&t.size.to_le_bytes());
            hasher.update(&t.hash.to_le_bytes())
        }

        hasher.digest128()
    }

    pub fn clone_with_meta_as_committed(&self, type_index: usize, meta: TypeMeta) -> Option<Entry> {
        let mut new_types = self.types.clone();
        let t = new_types.get_mut(type_index)?;
        t.meta = Some(meta);

        Some(Entry {
            committed: true,
            id: self.id,
            key: self.key,
            payloads_dir: self.payloads_dir.clone(),
            pending_types: self.pending_types.clone(),
            types: new_types,
        })
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        if !self.committed {
            self.drop_files();
        }
    }
}
