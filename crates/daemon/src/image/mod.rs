// use std::{collections::VecDeque, path::PathBuf, sync::{Arc, Condvar, Mutex}};

// pub struct ImageJob { pub id: u64, pub type_index: u32, pub source: ImageSource }
// pub enum ImageSource { Inline(Arc<[u8]>), File(PathBuf) }

// pub struct ImageQueue { state: Mutex<State>, ready: Condvar }
// struct State { jobs: VecDeque<ImageJob>, free_bytes: u64, closed: bool }
// impl ImageQueue {
//     pub fn push(&self, job: ImageJob);                  // push_back + notify_one
//     fn pop_newest(&self) -> Option<ImageJob>;           // wait while empty && !closed; pop_back; None when closed
//     fn reserve(&self, bytes: u64) -> Reservation<'_>;   // all-or-nothing: wait while free_bytes < bytes
//     fn try_reserve(&self, bytes: u64) -> Option<Reservation<'_>>;
//     pub fn close(&self);
// }
// struct Reservation<'a> { queue: &'a ImageQueue, bytes: u64 }  // Drop: free_bytes += bytes; notify_all (also on unwind)

// pub fn spawn_workers(queue: Arc<ImageQueue>, index_tx: Sender<IndexMsg>, snap: SnapshotHandle) -> io::Result<()>;
// fn run(queue: &ImageQueue, index_tx: &Sender<IndexMsg>, snap: &SnapshotHandle) {
//     if let Err(e) = set_current_thread_nice(NICE) { log_warn!("image worker nice: {e}") }
//     while let Some(job) = queue.pop_newest() {
//         if !snap.get_clone().items.iter().any(|i| i.entry.id == job.id) { continue }   // evicted
//         let src = match job.source {
//             ImageSource::Inline(b) => Src::Bytes(b),
//             ImageSource::File(p) => match File::open(&p) { Ok(f) => Src::File(f), Err(_) => continue }, // ENOENT = evicted
//         };
//         let meta = match panic::catch_unwind(AssertUnwindSafe(|| thumbnail(src, queue))) {
//             Ok(Ok(t)) => TypeMeta::Thumb(Some(Arc::new(t))),
//             Ok(Err(Fail::Transient(_))) => continue,           // EIO, ENOMEM, thread spawn failure: next start
//             Ok(Err(_)) | Err(_) => TypeMeta::Thumb(None),      // Format, OverCap, Timeout, TooBig, panic
//         };
//         if index_tx.send(IndexMsg::SetThumb { id: job.id, type_index: job.type_index, meta }).is_err() { break }
//     }
// }
