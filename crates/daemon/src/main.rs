#![forbid(unsafe_code)]
mod jobs;

use std::{
    io::{self, Error},
    os::fd::AsFd,
    process::ExitCode,
};

use clipleaf_core::{
    PollFd, WlBufferedStream, WlDataControlOffer, WlSessionEvent, WlSessionManager,
    ffi::{POLLIN, POLLNVAL, POLLOUT},
    init_logging, log_debug, log_error, log_info, poll,
};

use crate::jobs::{JobList, JobOutcome};

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

    let job_id = jobs.start("tester job", |cancel| {
        log_debug!("Log from inside job before sleep");
        poll(&mut [PollFd::new(cancel.as_fd(), POLLIN)], None).unwrap();
        log_debug!("Log from inside job after sleep");
        JobOutcome::Cancelled
    })?;

    log_debug!("Scheduled Job {}", job_id);
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
            jobs.on_job_done()?;
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
            jobs.cancel(job_id);
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
