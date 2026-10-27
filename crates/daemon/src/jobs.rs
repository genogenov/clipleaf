use std::{
    borrow::Cow,
    collections::VecDeque,
    fmt::Debug,
    io,
    os::fd::AsFd,
    sync::{
        Arc,
        mpsc::{self},
    },
    thread,
    time::{Duration, Instant},
};

use clipleaf_core::{ffi::event_fd::EventFd, log_debug, log_error};

pub const MAX_JOBS: usize = 8;

pub type JobId = u64;

#[derive(Debug)]
pub enum JobData {
    Fetch(Vec<(Cow<'static, str>, u64)>),
}

#[derive(Debug)]
pub enum JobOutcome {
    Cancelled,
    TimedOut,
    Failed(io::Error),
    Data(JobData),
}

#[derive(Debug)]
pub enum JobResult {
    Done(JobOutcome),
    Panicked,
}

pub struct JobMsg {
    pub id: JobId,
    pub result: JobResult,
    pub elapsed: Duration,
}

#[derive(Clone)]
pub struct JobPublish {
    tx: mpsc::Sender<JobMsg>,
    event: Arc<EventFd>,
}

pub struct JobSubscribe {
    rx: mpsc::Receiver<JobMsg>,
    event: Arc<EventFd>,
}

pub struct Job {
    id: JobId,
    cancel: Arc<EventFd>,
}

pub struct JobList {
    active: VecDeque<Job>,
    cancelling: Vec<Job>,
    next_id: u64,
    publish: JobPublish,
    subscribe: JobSubscribe,
}

pub struct JobGuard {
    id: JobId,
    publish: JobPublish,
    started: Instant,
    sent: bool,
}

impl JobGuard {
    pub fn finish(mut self, result: JobResult) -> io::Result<()> {
        if let Err(e) = self.publish.tx.send(JobMsg {
            id: self.id,
            result,
            elapsed: self.started.elapsed(),
        }) {
            return Err(io::Error::other(format!("failed to send JobMsg to receiver: {}", e)));
        }
        self.sent = true;
        self.publish.event.signal()?;
        Ok(())
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if !self.sent {
            _ = self.publish.tx.send(JobMsg {
                id: self.id,
                result: JobResult::Panicked,
                elapsed: self.started.elapsed(),
            });
            _ = self.publish.event.signal();
        }
    }
}

impl JobList {
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let signal = Arc::new(EventFd::new()?);
        Ok(Self {
            active: VecDeque::new(),
            cancelling: Vec::new(),
            next_id: 0,
            publish: JobPublish { tx, event: signal.clone() },
            subscribe: JobSubscribe { rx, event: signal },
        })
    }

    pub fn start<T>(&mut self, name: &str, body: T) -> io::Result<JobId>
    where
        T: Send + 'static,
        T: FnOnce(&EventFd) -> JobOutcome,
    {
        let cancel_ev = Arc::new(EventFd::new()?);
        let job_cancel = cancel_ev.clone();

        let id = self.next_id;
        let job = Job { id, cancel: cancel_ev };

        let guard_publish = self.publish.clone();
        let started = Instant::now();
        thread::Builder::new().name(format!("{name}-{}", job.id)).spawn(move || {
            let guard = JobGuard {
                id,
                publish: guard_publish,
                sent: false,
                started,
            };
            let outcome = body(&job_cancel);
            _ = guard.finish(JobResult::Done(outcome));
        })?;

        if self.active.len() == MAX_JOBS {
            let oldest_job = self.active.pop_front().unwrap();
            let old_id = oldest_job.id;
            let cancel_res = oldest_job.cancel.signal();
            self.cancelling.push(oldest_job);
            if let Err(e) = cancel_res {
                log_error!("Could not cancel job {} : {}", old_id, e);
            }
        }
        self.active.push_back(job);

        self.next_id = id.wrapping_add(1);
        Ok(id)
    }

    // pub fn cancel(&mut self, id: JobId) {
    //     if let Some(j_idx) = self.active.iter().position(|j| j.id == id)
    //         && let Some(job) = self.active.remove(j_idx)
    //     {
    //         let id = job.id;
    //         let cancel_res = job.cancel.signal();
    //         self.cancelling.push(job);
    //         if let Err(e) = cancel_res {
    //             log_error!("Could not cancel job {} : {}", id, e);
    //         }
    //         log_debug!("Job {} was cancelled", id);
    //     }
    // }

    pub fn reset_wait_event(&self) -> io::Result<u64> {
        self.subscribe.event.drain()
    }

    pub fn try_next_done(&mut self) -> Option<JobMsg> {
        while let Ok(msg) = self.subscribe.rx.try_recv() {
            if let Some(j_idx) = self.active.iter().position(|j| j.id == msg.id) {
                self.active.remove(j_idx); // drops the job's cancel eventfd
                log_debug!("Job {} completed with outcome {:?} in {:?}", msg.id, msg.result, msg.elapsed);

                return Some(msg);
            } else if let Some(j_idx) = self.cancelling.iter().position(|j| j.id == msg.id) {
                self.cancelling.swap_remove(j_idx);
                log_debug!(
                    "Job {} was cancelled and completed with outcome {:?} in {:?}",
                    msg.id,
                    msg.result,
                    msg.elapsed
                );
                return Some(msg);
            } else {
                log_error!("Job Id {} was not found in neither the active nor the cancelling jobs list", msg.id);
            }
        }

        None
    }
}

impl AsFd for JobList {
    fn as_fd(&self) -> std::os::unix::prelude::BorrowedFd<'_> {
        self.subscribe.event.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use std::io::PipeReader;

    use clipleaf_core::ffi::poll::{PollEvents, PollFd, PollResult, poll};

    use super::*;

    /// The next finished job, the way the main loop gets it: what is already queued first,
    /// then wait for the bell. The 10 s bound only turns a hang into a failure.
    fn next_done(jobs: &mut JobList) -> JobMsg {
        loop {
            if let Some(msg) = jobs.try_next_done() {
                return msg;
            }
            let mut fds = [PollFd::new(jobs.as_fd(), PollEvents::IN)];
            let result = poll(&mut fds, Some(Duration::from_secs(10))).unwrap();
            assert!(matches!(result, PollResult::Ready(1)), "no job finished");
            jobs.reset_wait_event().unwrap();
        }
    }

    /// Waits until the job is cancelled or `release` is closed.
    fn wait_for_cancel_or(cancel: &EventFd, release: &PipeReader) -> JobOutcome {
        let mut fds = [PollFd::new(cancel.as_fd(), PollEvents::IN), PollFd::new(release.as_fd(), PollEvents::IN)];
        poll(&mut fds, None).unwrap();
        if fds[0].revents().has_any(PollEvents::IN) {
            JobOutcome::Cancelled
        } else {
            JobOutcome::Data(JobData::Fetch(Vec::new()))
        }
    }

    #[test]
    fn a_finished_job_rings_the_bell_and_hands_over_its_result_once() {
        let mut jobs = JobList::new().unwrap();
        let id = jobs
            .start("test", |_| JobOutcome::Data(JobData::Fetch(vec![("text/plain".into(), 5)])))
            .unwrap();

        let msg = next_done(&mut jobs);
        assert_eq!(msg.id, id);
        let JobResult::Done(JobOutcome::Data(JobData::Fetch(types))) = msg.result else {
            panic!("{:?}", msg.result);
        };
        assert_eq!(types, [("text/plain".into(), 5)]);
        assert!(jobs.try_next_done().is_none());
    }

    #[test]
    fn a_job_that_panics_is_reported_as_panicked() {
        let mut jobs = JobList::new().unwrap();
        let id = jobs.start("test", |_| -> JobOutcome { panic!("job panics on purpose") }).unwrap();

        let msg = next_done(&mut jobs);
        assert_eq!(msg.id, id);
        assert!(matches!(msg.result, JobResult::Panicked), "{:?}", msg.result);
    }

    #[test]
    fn starting_one_job_too_many_cancels_the_oldest_and_the_rest_keep_running() {
        let mut jobs = JobList::new().unwrap();
        let (release, release_all) = io::pipe().unwrap();
        let ids: Vec<JobId> = (0..=MAX_JOBS)
            .map(|_| {
                let release = release.try_clone().unwrap();
                jobs.start("test", move |cancel| wait_for_cancel_or(cancel, &release)).unwrap()
            })
            .collect();
        assert_eq!(ids, (0..=MAX_JOBS as JobId).collect::<Vec<_>>());

        // Nothing else can finish before the release, so the first message is the oldest job.
        let msg = next_done(&mut jobs);
        assert_eq!(msg.id, ids[0]);
        assert!(matches!(msg.result, JobResult::Done(JobOutcome::Cancelled)), "{:?}", msg.result);

        drop(release_all);
        let mut finished: Vec<JobId> = (0..MAX_JOBS)
            .map(|_| {
                let msg = next_done(&mut jobs);
                assert!(matches!(msg.result, JobResult::Done(JobOutcome::Data(_))), "{:?}", msg.result);
                msg.id
            })
            .collect();
        finished.sort_unstable();
        assert_eq!(finished, ids[1..]);
    }
}
