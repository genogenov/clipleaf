#![forbid(unsafe_code)]
mod jobs;
mod offers_fetch;

use core::slice;
use std::{
    borrow::Cow,
    io::{self, Error, PipeReader, Read},
    iter, mem,
    os::fd::AsFd,
    process::ExitCode,
    time::{Duration, Instant},
};

use clipleaf_core::{
    EventFd, PollFd, PollResult, WlBufferedStream, WlDataControlOffer, WlSessionEvent,
    WlSessionManager,
    ffi::{POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT},
    init_logging, log_debug, log_error, log_info, pick_fetch_mimes, poll,
};

use crate::{jobs::{JobData, JobList, JobOutcome, JobResult}, offers_fetch::start_fetch};

const MAX_KNOWN_RETAINED_OFFERS: usize = 10;
const FETCH_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

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

fn run() -> io::Result<()> {
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
                    res => log_error!("job {} failed: {res:?}", msg.id), // Failed, Panicked
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
            if let Err(e) = start_fetch(&mut session, &mut jobs, FETCH_IDLE_TIMEOUT) {
                log_error!("Failed to start fetch offer job: {e}");
            }
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
