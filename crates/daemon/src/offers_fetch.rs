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
            session.receive_offer(false, Some(mime), tx.into())?;
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
        .zip(pipes)
        .map(|(mime, pipe)| FetchOp {
            mime,
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

#[cfg(test)]
mod tests {
    use std::{
        io::{PipeWriter, Write},
        thread,
    };

    use super::*;

    /// One pipe per type; the test plays the source on the write ends.
    fn sources(
        names: &[&'static str],
    ) -> (Vec<PipeReader>, Vec<Cow<'static, str>>, Vec<PipeWriter>) {
        let (readers, writers) = names.iter().map(|_| io::pipe().unwrap()).unzip();
        (
            readers,
            names.iter().map(|n| Cow::Borrowed(*n)).collect(),
            writers,
        )
    }

    fn fetched(outcome: JobOutcome) -> Vec<(Cow<'static, str>, u64)> {
        match outcome {
            JobOutcome::Data(JobData::Fetch(types)) => types,
            other => panic!("{other:?}"),
        }
    }

    const IDLE: Duration = Duration::from_secs(10); // only bounds a hang; never reached

    #[test]
    fn every_type_is_read_to_its_end_and_keeps_its_own_name_whatever_order_they_finish_in() {
        let (readers, names, mut writers) = sources(&["text/plain", "image/png"]);
        let big = writers.pop().unwrap();
        // More than a pipe holds, so the fetch has to read while the source writes.
        let image = thread::spawn(move || {
            let mut big = big;
            big.write_all(&vec![7u8; 300 * 1024]).unwrap();
        });
        let mut text = writers.pop().unwrap();
        text.write_all(b"hello").unwrap();
        drop(text);

        let cancel = EventFd::new().unwrap();
        let types = fetched(fetch(readers, names, &cancel, IDLE));
        image.join().unwrap();
        assert_eq!(
            types,
            [("text/plain".into(), 5), ("image/png".into(), 300 * 1024)]
        );
    }

    #[test]
    fn empty_types_are_left_out_and_all_empty_is_an_empty_result() {
        let (readers, names, mut writers) = sources(&["text/plain", "text/html"]);
        writers[1].write_all(b"<b>").unwrap();
        drop(writers);
        let cancel = EventFd::new().unwrap();
        assert_eq!(
            fetched(fetch(readers, names, &cancel, IDLE)),
            [("text/html".into(), 3)]
        );

        let (readers, names, writers) = sources(&["text/plain", "text/html"]);
        drop(writers);
        assert!(fetched(fetch(readers, names, &cancel, IDLE)).is_empty());
    }

    #[test]
    fn a_silent_source_times_out_and_the_types_that_finished_are_dropped_too() {
        // Never partial: one type that never ends loses the whole copy.
        let (readers, names, mut writers) = sources(&["text/plain", "image/png"]);
        writers[0].write_all(b"done").unwrap();
        drop(writers.remove(0));
        let _silent = writers; // stays open, never written

        let cancel = EventFd::new().unwrap();
        let outcome = fetch(readers, names, &cancel, Duration::from_millis(50));
        assert!(matches!(outcome, JobOutcome::TimedOut), "{outcome:?}");
    }

    #[test]
    fn a_cancelled_fetch_stops_without_waiting_for_the_source() {
        let (readers, names, _silent) = sources(&["text/plain"]);
        let cancel = EventFd::new().unwrap();
        cancel.signal().unwrap();
        let outcome = fetch(readers, names, &cancel, IDLE);
        assert!(matches!(outcome, JobOutcome::Cancelled), "{outcome:?}");
    }

    #[test]
    fn a_source_that_keeps_sending_is_never_cut_off_however_long_it_takes() {
        // The timeout counts silence, not the whole fetch: 600 ms of steady 20 ms gaps against a
        // 300 ms idle timeout. Only a gap 15x the planned one could fail this.
        let (readers, names, mut writers) = sources(&["text/plain"]);
        let mut slow = writers.pop().unwrap();
        let source = thread::spawn(move || {
            for _ in 0..30 {
                slow.write_all(b"x").unwrap();
                thread::sleep(Duration::from_millis(20));
            }
        });
        let cancel = EventFd::new().unwrap();
        let outcome = fetch(readers, names, &cancel, Duration::from_millis(300));
        source.join().unwrap();
        assert_eq!(fetched(outcome), [("text/plain".into(), 30)]);
    }
}
