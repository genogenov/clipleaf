#![forbid(unsafe_code)]
mod index;
mod jobs;
mod offers_fetch;
mod storage;

use std::{
    io::{self, Error},
    os::fd::AsFd,
    process::ExitCode,
    sync::mpsc::Sender,
    time::Duration,
};

use clipleaf_core::{
    PollFd, WlBufferedStream, WlDataControlOffer, WlSessionEvent, WlSessionManager,
    ffi::{POLLIN, POLLNVAL, POLLOUT},
    init_logging, log_debug, log_error, log_info, poll,
};

use crate::{
    jobs::{JobData, JobList, JobOutcome, JobResult},
    offers_fetch::start_fetch,
    storage::{
        FsDataStore,
        wal::log::{LogMsg, Restored, WriteAheadLog},
    },
};

const FETCH_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HISTORY_LIMIT: usize = 100;

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
    log_debug!("Opening WAL file..");
    let mut log = WriteAheadLog::open(store.wal_dir(), store.payloads_dir())?;
    log_debug!("Initiating WAL restore..");
    let restored = log.restore()?;
    *seq_counter = restored.next_seq;
    log_debug!("Spawning WAL..");
    let log_tx = log.start()?;
    Ok((log_tx, restored))
}

fn run() -> io::Result<()> {
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
    log_debug!("Spawning index..");
    let (index_tx, _snapshot) = index::spawn_index(MAX_HISTORY_LIMIT, log_tx, restored)?;

    let mut stream = WlBufferedStream::connect_from_env()?;
    stream.set_nonblocking_writes();
    let mut session = WlSessionManager::initialize(stream)?;

    let mut jobs = JobList::new()?;

    loop {
        session.flush()?; // one sendmsg per iteration
        let events = POLLIN
            | if session.has_pending_writes() {
                POLLOUT
            } else {
                0
            };
        let (wl_events, job_events) = {
            let mut fds = [
                PollFd::new(session.as_fd(), events),
                PollFd::new(jobs.as_fd(), POLLIN),
            ];
            poll(&mut fds, None)?;
            (fds[0].revents(), fds[1].revents())
        }; // borrow ends before the drain
        if wl_events & POLLNVAL != 0 {
            return Err(Error::other("wayland socket not open"));
        } // our bug

        if job_events & POLLNVAL != 0 {
            return Err(Error::other("job bell not open"));
        }
        if job_events & POLLIN != 0 {
            jobs.reset_wait_event()?;
            while let Some(msg) = jobs.try_next_done() {
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
        let mut changed_offer = false;
        let mut changed_primary_offer = false;
        while let Some(event) = session.try_next_event()? {
            if let WlSessionEvent::Selection { .. } = event {
                changed_offer = true;
            } // primary: ignored for now
            if let WlSessionEvent::PrimarySelection { .. } = event {
                changed_primary_offer = true;
            }
        }
        if changed_offer {
            log_selection(session.get_offer());
            if let Err(e) = start_fetch(
                &mut session,
                &mut jobs,
                FETCH_IDLE_TIMEOUT,
                seq_counter,
                store.payloads_dir(),
                index_tx.clone(),
            ) {
                log_error!("Failed to start fetch offer job: {e}");
            }
            seq_counter = seq_counter.wrapping_add(1);
        }
        if changed_primary_offer {
            log_selection(session.get_primary_offer());
        }
    }
}

fn log_selection(offer: Option<&WlDataControlOffer>) {
    match offer {
        Some(o) => {
            log_info!("Offered mime types: {}", o.offered_mime_types().join(", "));
        }
        None => log_info!("selection cleared"),
    }
}
