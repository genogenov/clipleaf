use std::{
    borrow::Cow,
    fmt::Debug,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use clipleaf_core::{Mime, MimeKind, log_debug, log_error};
use xxhash_rust::xxh3::Xxh3;

use crate::storage::{Data, EntryBuilder, MAX_INLINE_TYPE_BYTES, TypeData};

pub struct PayloadFile {
    pub(super) path: PathBuf,
    head: Option<Arc<[u8]>>,
    //seqnum: usize
}

pub struct PayloadWriter {
    pub(super) file_path_base: Arc<Path>,
    pub(super) mime: Option<Mime<Cow<'static, str>>>,
    pub(super) file_id: u64,
    pub(super) file_seqnum: usize,
    pub(super) size: u64,
    pub(super) head: Option<Vec<u8>>,
    pub(super) hasher: Xxh3,
    pub(super) file: Option<(PayloadFile, File)>,
}

impl PayloadWriter {
    pub fn write_all(&mut self, buff: &[u8]) -> io::Result<()> {
        self.hasher.update(buff);

        let head = self.head.as_mut().unwrap();

        let (fits, rest) = buff.split_at((MAX_INLINE_TYPE_BYTES - head.len()).min(buff.len()));
        head.extend_from_slice(fits);

        if !rest.is_empty() {
            let file = match &mut self.file {
                Some(f) => f,
                None => {
                    let result = self.file.insert(PayloadFile::create_write(
                        &self.file_path_base,
                        self.file_id,
                        self.file_seqnum,
                    )?);
                    log_debug!("Created payload file at {}", result.0.path.display());
                    result.1.write_all(head)?;
                    result
                }
            };
            file.1.write_all(rest)?;
        }
        self.size += buff.len() as u64;
        Ok(())
    }

    pub fn mime(&self) -> &Mime<Cow<'static, str>> {
        self.mime.as_ref().unwrap()
    }

    pub fn finish(mut self, builder: &mut EntryBuilder) {
        let mime = self.mime.take().unwrap();
        let needs_head = match mime.kind() {
            MimeKind::FileList | MimeKind::Text | MimeKind::TextExtended | MimeKind::TextLike => {
                true
            }
            MimeKind::Image => false,
            MimeKind::Other => false,
            MimeKind::Password => false,
        };
        let data = TypeData {
            hash: self.hasher.digest128(),
            mime,
            size: self.size,
            data: match self.file.take() {
                None => Data::Inline(Arc::from(self.head.take().unwrap())),
                Some(mut f) => {
                    if needs_head {
                        f.0.head = self.head.take().map(Arc::from);
                    }
                    Data::File(f.0)
                }
            },
        };

        builder.entry.pending_types[self.file_seqnum] = Some(data);
    }
}

impl Drop for PayloadWriter {
    fn drop(&mut self) {
        if let Some((payload, _file)) = self.file.take() {
            // still here means not finished
            log_debug!("Dropping unfinished file at {}", payload.path.display());
            if let Err(e) = fs::remove_file(&payload.path) {
                log_error!("…: {e}");
            }
        }
    }
}

impl PayloadFile {
    pub fn create_write(dir: &Path, id: u64, seqnum: usize) -> io::Result<(Self, File)> {
        let path = dir.join(format!("{}.{}", id, seqnum));
        let file = OpenOptions::new()
            .mode(0o600)
            .write(true)
            .create_new(true)
            .open(&path)?;

        let paylad = Self {
            head: None,
            path,
            //seqnum: seqnum
        };

        Ok((paylad, file))
    }
}

impl Debug for PayloadFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayloadFile")
            .field("path", &self.path)
            .field("head", &self.head.as_ref().map(|h| h.len()))
            .finish()
    }
}
