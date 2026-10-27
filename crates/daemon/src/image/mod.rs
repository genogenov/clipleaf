mod encoder;

use std::{
    fs::File,
    io::{self},
    panic::{self, AssertUnwindSafe},
    path::PathBuf,
    sync::{Arc, mpsc::Sender},
    thread,
    time::Instant,
};

use clipleaf_core::{ffi::thread::lower_current_thread_priority, log_debug, log_error, log_warn};

use crate::{
    image::encoder::{ImageProcessingFail, ImageSourceReader, make_thumbnail},
    index::{IndexMsg, SnapshotHandle},
    storage::TypeMeta,
    work_queue::WorkQueue,
};

const LOWER_THREAD_PRIORITY_BY: u8 = 10;
const PNG_MAX_WIDTH: u32 = 32 * 1024;
const PNG_MAX_DECODED_SIZE: u64 = 100 * 1024 * 1024;
const JPEG_MAX_DECODED_SIZE: u64 = 100 * 1024 * 1024;
const JPEG_MAX_WIDTH: u32 = 8 * 1024;
const JPEG_MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;
const MAX_INPUT_IMG_HEIGHT: u32 = 64 * 1024;
const MAX_TOTAL_ALLOC: u64 = 5 * 1024 * 1024;
const THUMBNAIL_BOX_SIZE: u32 = 256;
const THUMBNAIL_QUALITY: u8 = 70;
const WORKERS: u8 = 2;

pub struct ImageJob {
    pub id: u64,
    pub type_index: u32,
    pub source: ImageSource,
}
pub enum ImageSource {
    Inline(Arc<[u8]>),
    File(PathBuf),
}

pub fn spawn_workers(queue: Arc<WorkQueue<ImageJob>>, index_tx: Sender<IndexMsg>, snap: SnapshotHandle) -> io::Result<()> {
    for i in 0..WORKERS {
        let (queue, tx, snap) = (queue.clone(), index_tx.clone(), snap.clone());
        thread::Builder::new()
            .name(format!("image-thumb-{i}"))
            .spawn(move || run(&queue, &tx, &snap))?;
    }
    Ok(())
}

fn run(queue: &WorkQueue<ImageJob>, index_tx: &Sender<IndexMsg>, snap: &SnapshotHandle) {
    if let Err(e) = lower_current_thread_priority(LOWER_THREAD_PRIORITY_BY) {
        log_warn!("image worker nice: {e}")
    }
    while let Some(job) = queue.pop_newest() {
        let start = Instant::now();
        if !snap.get_clone().items.iter().any(|i| i.entry.id == job.id) {
            continue;
        }

        let meta = match panic::catch_unwind(AssertUnwindSafe(|| match job.source {
            ImageSource::Inline(b) => {
                let res = make_thumbnail(ImageSourceReader::from_bytes(b));
                log_debug!("Img worker: Produced img in {:?}", start.elapsed());
                res
            }
            ImageSource::File(p) => match File::open(&p) {
                Ok(f) => {
                    let res = make_thumbnail(ImageSourceReader::from_file(f)?);
                    log_debug!("Img worker: Produced img in {:?}", start.elapsed());
                    res
                }
                Err(e) => Err(e.into()),
            },
        })) {
            Ok(Ok(t)) => TypeMeta::ThumbNail(Some(Arc::new(t))),
            Ok(Err(ImageProcessingFail::TransientIO(e))) => {
                log_warn!("Transient IO error during image processing: {e}");
                continue;
            }
            Ok(Err(ImageProcessingFail::Permanent(e))) => {
                log_error!("Permanent error during image processing: {e}");
                TypeMeta::ThumbNail(None)
            }
            Ok(Err(ImageProcessingFail::Unsupported(e))) => {
                log_debug!("Unsupported image payload for thumbnail generation: {e}");
                TypeMeta::ThumbNail(None)
            }
            Ok(Err(ImageProcessingFail::PermanentIO(e))) => {
                log_warn!("Permanent IO error during image processing: {e}");
                TypeMeta::ThumbNail(None)
            }
            Err(panic_err) => {
                let msg = panic_err
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic_err.downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("non-string panic payload");
                log_error!("Panic during image processing: {msg}");
                TypeMeta::ThumbNail(None)
            }
        };
        if index_tx
            .send(IndexMsg::SetThumb {
                id: job.id,
                type_index: job.type_index,
                meta,
            })
            .is_err()
        {
            break;
        }
    }
}
