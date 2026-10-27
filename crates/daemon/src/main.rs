#![forbid(unsafe_code)]
mod image;
mod index;
mod jobs;
mod offer;
mod storage;
mod work_queue;

use std::{
    io::{self, Error},
    os::fd::AsFd,
    process::ExitCode,
    sync::{Arc, mpsc::Sender},
    time::Duration,
};

use clipleaf_core::{
    WlBufferedStream, WlSessionEvent, WlSessionManager,
    ffi::poll::{PollEvents, PollFd, poll},
    init_logging, log_debug, log_error, log_info, pick_fetch_mimes,
};

use crate::{
    index::SnapshotHandle,
    jobs::{JobData, JobList, JobOutcome, JobResult},
    offer::{
        DataOwner, MAX_FETCHED_TYPES, MAX_PENDING_SERVES, RESTORE_ON_CLEAR, RESTORE_ON_START,
        TRACK_PRIMARY_SELECTION,
        offers_fetch::FetchJobDispatcher,
        serve::{ServeJob, spawn_servers},
    },
    storage::{
        FsDataStore,
        wal::log::{LogMsg, Restored, WriteAheadLog},
    },
    work_queue::WorkQueue,
};

const FETCH_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HISTORY_LIMIT: usize = 100;

struct Daemon {
    seq_counter: u64,
    serve_queue: Arc<WorkQueue<ServeJob>>,
    data_owner: DataOwner,
    session: WlSessionManager,
    jobs: JobList,
    snapshot: SnapshotHandle,
    fetch_dispatcher: FetchJobDispatcher,
    _store: FsDataStore,
}
impl Daemon {
    fn init() -> io::Result<Self> {
        let mut seq_counter: u64 = 1;
        let mut store = FsDataStore::open_data_store_dir(None)?;
        store.lock()?;

        let (log_tx, restored) = match init_wal(&store, &mut seq_counter) {
            Ok(it) => it,
            Err(err) => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("Failed to span WAL: {}", err),
                ));
            }
        };

        let img_queue = Arc::new(WorkQueue::new());
        let serve_queue = Arc::new(WorkQueue::new());

        log_debug!("Spawning serve threads..");
        spawn_servers(serve_queue.clone())?;

        log_debug!("Spawning index..");
        let (index_tx, snapshot) = index::spawn_index(
            MAX_HISTORY_LIMIT,
            log_tx.clone(),
            restored,
            img_queue.clone(),
            store.payloads_dir(),
        )?;

        log_debug!("Spawnig Img processing threads..");
        image::spawn_workers(img_queue.clone(), index_tx.clone(), snapshot.clone())?;

        let data_owner = DataOwner::new();

        let mut stream = WlBufferedStream::connect_from_env()?;
        stream.set_nonblocking_writes();
        let session = WlSessionManager::initialize(stream)?;
        let jobs = JobList::new()?;
        let payloads_dir = store.payloads_dir();
        Ok(Self {
            data_owner,
            jobs,
            seq_counter,
            serve_queue,
            session,
            snapshot,
            fetch_dispatcher: FetchJobDispatcher::new(payloads_dir, index_tx, FETCH_IDLE_TIMEOUT),
            _store: store,
        })
    }

    fn handle_selection_changed(&mut self, first_selection_event: bool, is_primary: bool) {
        log_debug!("Handling selection - is primary: {is_primary}");
        let offer = {
            if !is_primary {
                self.session.get_offer()
            } else {
                self.session.get_primary_offer()
            }
        };
        match offer {
            Some(offer) => {
                if self.data_owner.is_ours(offer) {
                    log_debug!("Detected our offer. Skipping.");
                    return;
                }
                let offered_mimes = offer.offered_mime_types();
                let fetch_mimes = pick_fetch_mimes(offered_mimes, MAX_FETCHED_TYPES);
                if let Err(e) = self.fetch_dispatcher.start_fetch(
                    &mut self.session,
                    is_primary,
                    fetch_mimes,
                    &mut self.jobs,
                    self.seq_counter,
                ) {
                    log_error!("Failed to start fetch offer job: {e}");
                }
                self.seq_counter = self.seq_counter.wrapping_add(1);
            }
            None => {
                if (first_selection_event && RESTORE_ON_START)
                    || (!first_selection_event && RESTORE_ON_CLEAR)
                {
                    log_debug!("Restoring last history entry");
                    if let Some(item) = self.snapshot.get_clone().items.first()
                        && let Err(e) = self.data_owner.restore(
                            &mut self.session,
                            item.entry.clone(),
                            is_primary,
                        )
                    {
                        log_error!("Failed to restore selection to last history entry: {e}");
                    }
                }
            }
        }
    }
}

fn main() -> ExitCode {
    init_logging();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log_error!("{}", err);
            ExitCode::FAILURE
        }
    }
}

fn init_wal(store: &FsDataStore, seq_counter: &mut u64) -> io::Result<(Sender<LogMsg>, Restored)> {
    log_debug!("Opening WAL file and restoring..");
    let (log, restored) = WriteAheadLog::open_restore(store.wal_dir(), store.payloads_dir())?;
    *seq_counter = restored.next_seq;
    log_debug!("Spawning WAL..");
    let log_tx = log.start()?;
    Ok((log_tx, restored))
}

fn run() -> io::Result<()> {
    let mut daemon = Daemon::init()?;

    let mut first_selection_event = true;
    let mut first_primary_selection_event = true;
    loop {
        daemon.session.flush()?; // one sendmsg per iteration
        let events = PollEvents::IN
            | if daemon.session.has_pending_writes() {
                PollEvents::OUT
            } else {
                PollEvents::EMPTY
            };
        let (wl_events, job_events) = {
            let mut fds = [
                PollFd::new(daemon.session.as_fd(), events),
                PollFd::new(daemon.jobs.as_fd(), PollEvents::IN),
            ];
            poll(&mut fds, None)?;
            (fds[0].revents(), fds[1].revents())
        }; // borrow ends before the drain
        if wl_events.has_any(PollEvents::NVAL) {
            return Err(Error::other("wayland socket not open"));
        } // our bug

        if job_events.has_any(PollEvents::NVAL) {
            return Err(Error::other("job bell not open"));
        }
        if job_events.has_any(PollEvents::IN) {
            daemon.jobs.reset_wait_event()?;
            while let Some(msg) = daemon.jobs.try_next_done() {
                match msg.result {
                    JobResult::Done(JobOutcome::Data(JobData::Fetch(fetch_result))) => {
                        log_info!("job {}: {fetch_result:?} in {:?}", msg.id, msg.elapsed)
                    }
                    JobResult::Done(JobOutcome::Cancelled) => {
                        log_debug!("job {} cancelled", msg.id)
                    }
                    JobResult::Done(JobOutcome::TimedOut) => {
                        log_info!("job {}: source went silent, entry dropped", msg.id)
                    }
                    JobResult::Done(JobOutcome::Failed(e)) => {
                        log_error!("job {} failed: {e}", msg.id)
                    }
                    JobResult::Panicked => log_error!("job {} panicked", msg.id),
                }
            }
        }

        // POLLIN, POLLHUP, POLLERR: drain. HUP/ERR end in Err(UnexpectedEof) from the drain.

        let (mut clipboard_changed, mut primary_changed) = (false, false);
        while let Some(event) = daemon.session.try_next_event()? {
            match event {
                WlSessionEvent::Selection { .. } => {
                    clipboard_changed = true;
                    daemon.serve_queue.retain(|j| j.is_primary);
                }
                WlSessionEvent::PrimarySelection { .. } if TRACK_PRIMARY_SELECTION => {
                    primary_changed = true;
                    daemon.serve_queue.retain(|j| !j.is_primary);
                }
                WlSessionEvent::DataControlSend { source, mime, fd } => {
                    if let Some(job) = daemon.data_owner.on_send(source, &mime, fd)
                        && daemon
                            .serve_queue
                            .try_push(job, MAX_PENDING_SERVES)
                            .is_none()
                    {
                        log_debug!(
                            "The limit of pending serve jobs has been reached: {MAX_PENDING_SERVES}. Dropping new jobs until existing jobs drop below the limit."
                        );
                    }
                }
                WlSessionEvent::DataControlCancelled { source } => {
                    daemon
                        .data_owner
                        .on_cancelled(&mut daemon.session, source)?;
                }
                _ => {}
            }
        }

        if clipboard_changed {
            daemon.handle_selection_changed(first_selection_event, false);
            first_selection_event = false;
        }
        if primary_changed {
            daemon.handle_selection_changed(first_primary_selection_event, true);
            first_primary_selection_event = false;
        }
    }
}
