use std::{
    borrow::Cow,
    io::{self, PipeReader, Read},
    iter,
    os::fd::AsFd,
    time::Duration,
};

use clipleaf_core::{
    EventFd, PollFd, PollResult, WlSessionManager,
    ffi::{POLLERR, POLLHUP, POLLIN},
    log_error, pick_fetch_mimes, poll,
};

use crate::jobs::{JobData, JobList, JobOutcome};

/// Upper bound on the types fetched per copy; the default pick yields at most 6 (text + image + 4 file lists).
const MAX_FETCHED_TYPES: usize = 10;

struct FetchOp {
    mime: Cow<'static, str>,
    pipe: Option<PipeReader>,
    bytes: u64,
}

pub fn start_fetch(
    session: &mut WlSessionManager,
    jobs: &mut JobList,
    idle_timeout: Duration,
) -> io::Result<()> {
    if let Some(mimes) = session
        .get_offer()
        .map(|o| pick_fetch_mimes(o.offered_mime_types(), MAX_FETCHED_TYPES))
        && !mimes.is_empty()
    {
        let mut pipes = Vec::new();
        for mime in &mimes {
            let (rx, tx) = io::pipe()?;
            session.receive_offer(false, Some(&mime), tx.into())?;
            pipes.push(rx);
        }
        jobs.start("fetch-selection", move |cancel| {
            fetch(pipes, mimes, cancel, idle_timeout)
        })?;
    }
    Ok(())
}

fn fetch(
    pipes: Vec<PipeReader>,
    mimes: Vec<Cow<'static, str>>,
    cancel: &EventFd,
    max_idle: Duration,
) -> JobOutcome {
    let mut revents = Vec::with_capacity(pipes.len() + 1);
    let cancel_poll = PollFd::new(cancel.as_fd(), POLLIN);
    let mut fetch_ops = mimes
        .into_iter()
        .zip(pipes.into_iter())
        .map(|(mime, pipe)| FetchOp {
            mime: mime,
            pipe: Some(pipe),
            bytes: 0,
        })
        .collect::<Vec<_>>();

    let mut buff = vec![0u8; 1024 * 64];

    loop {
        let poll_result = {
            let mut fds = iter::once(cancel_poll)
                .chain(
                    fetch_ops
                        .iter()
                        .filter_map(|o| o.pipe.as_ref().map(|p| PollFd::new(p.as_fd(), POLLIN))),
                )
                .collect::<Vec<_>>();

            if fds.len() == 1 {
                // Every pipe reached EOF: a read error returns early (never partial).
                let result = fetch_ops
                    .into_iter()
                    .filter(|o| o.bytes > 0)
                    .map(|op| (op.mime, op.bytes))
                    .collect();

                return JobOutcome::Data(JobData::Fetch(result));
            }

            let r = match poll(&mut fds, Some(max_idle)) {
                Ok(r) => r,
                Err(e) => {
                    return JobOutcome::Failed(e);
                }
            };
            revents.clear();
            revents.extend(fds.iter().map(|f| f.revents()));
            r
        };
        match poll_result {
            PollResult::Ready(_) => {
                if revents[0] & POLLIN != 0 {
                    return JobOutcome::Cancelled;
                }
                for (i, op) in fetch_ops
                    .iter_mut()
                    .filter(|op| op.pipe.is_some())
                    .enumerate()
                {
                    let active = revents[i + 1] & (POLLIN | POLLHUP | POLLERR) != 0;
                    if active {
                        let reader = op.pipe.as_mut().unwrap();
                        match reader.read(&mut buff) {
                            Ok(0) => op.pipe = None,
                            Ok(n) => {
                                op.bytes += n as u64;
                            }
                            Err(e) => {
                                log_error!(
                                    "Failed to read from pipe for mime type {} : {}",
                                    op.mime,
                                    e
                                );
                                return JobOutcome::Failed(e);
                            }
                        }
                    }
                }
            }
            PollResult::Timeout => {
                return JobOutcome::TimedOut;
            }
        }
    }
}
