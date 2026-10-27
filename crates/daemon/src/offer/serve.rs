use std::{
    fs::File,
    io::{self, ErrorKind},
    os::fd::OwnedFd,
    panic::{self, AssertUnwindSafe},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::{
    log_debug, log_error, log_warn, serve_bytes_nonblocking, serve_file_nonblocking,
};

use crate::{
    storage::{Data, Entry},
    work_queue::WorkQueue,
};

const SERVE_STALL: Duration = Duration::from_secs(5);

const WORKERS: u8 = 4;

pub struct ServeJob {
    pub entry: Arc<Entry>,
    pub type_index: usize,
    pub fd: OwnedFd,
    pub is_primary: bool,
}

pub fn spawn_servers(queue: Arc<WorkQueue<ServeJob>>) -> io::Result<()> {
    for i in 0..WORKERS {
        let q = queue.clone();
        thread::Builder::new()
            .name(format!("serve-{i}"))
            .spawn(move || run(&q))?;
    }
    Ok(())
}

fn run(queue: &WorkQueue<ServeJob>) {
    while let Some(job) = queue.pop_oldest() {
        let start = Instant::now();
        // A panic in one paste must not shrink the pool for good: the job and its fd are
        // dropped while unwinding (the paster sees EOF), and this worker takes the next job.
        match panic::catch_unwind(AssertUnwindSafe(|| serve(job))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) if matches!(e.kind(), ErrorKind::BrokenPipe | ErrorKind::TimedOut) => {
                log_debug!("Serve: paster gone: {e}")
            }
            Ok(Err(e)) => log_error!("Serve: failed serving job: {e}"),
            Err(_) => log_error!("Serve: a serve job panicked; its paste was dropped"),
        }
        log_debug!("Serve: Finished serve job in {:?}", start.elapsed());
    }
}

/// One paste. A type or payload file that is gone is logged here and is not an error: the
/// paster just sees EOF when the fd drops.
fn serve(job: ServeJob) -> io::Result<()> {
    let Some(t) = job.entry.types.get(job.type_index) else {
        log_warn!("No entry type data at index {}. Skipping.", job.type_index);
        return Ok(());
    };
    match &t.data {
        Data::Inline(bytes) => serve_bytes_nonblocking(job.fd, bytes, SERVE_STALL),
        Data::File(f) => {
            let path = f.payload_path(&job.entry.payloads_dir, job.entry.id);
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    log_warn!(
                        "Attempt to serve non existent file from path: {}; Skipping.",
                        &path.display()
                    );
                    return Ok(());
                }
                Err(e) => {
                    log_error!(
                        "Serve: failed serving file at path {} : {}",
                        &path.display(),
                        e
                    );
                    return Ok(());
                }
            };
            serve_file_nonblocking(job.fd, &file, t.size, SERVE_STALL)
        }
    }
}
