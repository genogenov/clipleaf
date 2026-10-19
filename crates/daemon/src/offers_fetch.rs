use std::{
    borrow::Cow,
    io::{self, PipeReader, Read},
    iter,
    os::fd::AsFd,
    path::Path,
    sync::mpsc::Sender,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clipleaf_core::{
    EventFd, Mime, PollFd, PollResult, WlSessionManager,
    ffi::{POLLERR, POLLHUP, POLLIN},
    log_error, pick_fetch_mimes, poll,
};

use crate::{
    index::IndexMsg,
    jobs::{JobData, JobList, JobOutcome},
    storage::{EntryBuilder, PayloadWriter},
};

/// Upper bound on the types fetched per copy; the default pick yields at most 6 (text + image + 4 file lists).
const MAX_FETCHED_TYPES: usize = 10;

struct FetchOp {
    pipe: Option<PipeReader>,
    writer: Option<PayloadWriter>,
}

pub fn start_fetch(
    session: &mut WlSessionManager,
    jobs: &mut JobList,
    idle_timeout: Duration,
    id_seq: u64,
    payloads_dir: &Path,
    index_tx: Sender<IndexMsg>,
) -> io::Result<()> {
    if let Some(mimes) = session
        .get_offer()
        .map(|o| pick_fetch_mimes(o.offered_mime_types(), MAX_FETCHED_TYPES))
        && !mimes.is_empty()
    {
        let mut pipes = Vec::new();
        for mime in &mimes {
            let (rx, tx) = io::pipe()?;
            session.receive_offer(false, Some(mime.mime()), tx.into())?;
            pipes.push(rx);
        }

        let entry = EntryBuilder::new(id_seq, payloads_dir);

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)) // saturate instead of wrapping
            .unwrap_or(0);

        jobs.start("fetch-selection", move |cancel| {
            fetch(
                (pipes, mimes),
                entry,
                cancel,
                idle_timeout,
                index_tx,
                id_seq,
                now_ms,
            )
        })?;
    }
    Ok(())
}

fn fetch(
    pipes: (Vec<PipeReader>, Vec<Mime<Cow<'static, str>>>),
    entry: EntryBuilder,
    cancel: &EventFd,
    max_idle: Duration,
    index_tx: Sender<IndexMsg>,
    id_seq: u64,
    now_ms: u64,
) -> JobOutcome {
    let mut entry = entry;
    let mut revents = Vec::with_capacity(pipes.0.len() + 1);
    let cancel_poll = PollFd::new(cancel.as_fd(), POLLIN);
    let mut fetch_ops = Vec::with_capacity(pipes.0.len());
    for (pipe, mime) in pipes.0.into_iter().zip(pipes.1) {
        let payload = entry.payload(mime);
        fetch_ops.push(FetchOp {
            pipe: Some(pipe),
            writer: Some(payload),
        });
    }

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
                let entry = entry.create();
                let result: Vec<(Cow<'_, str>, u64)> = entry
                    .types()
                    .iter()
                    .map(|op| (op.mime().mime().clone(), op.size()))
                    .collect();

                if !result.is_empty()
                    && let Err(e) = index_tx.send(IndexMsg::Insert {
                        seq: id_seq,
                        time_ms: now_ms,
                        entry,
                    })
                {
                    return JobOutcome::Failed(io::Error::other(format!(
                        "Failed to send msg to Indexer: {e}"
                    )));
                }
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
                        let writer = op.writer.as_mut().unwrap();
                        match reader.read(&mut buff) {
                            Ok(0) => {
                                op.pipe = None;
                                op.writer.take().unwrap().finish(&mut entry);
                            }
                            Ok(n) => {
                                if let Err(e) = writer.write_all(&buff[..n]) {
                                    log_error!("Failed to store {} : {}", writer.mime().mime(), e);
                                    return JobOutcome::Failed(e);
                                }
                            }
                            Err(e) => {
                                log_error!(
                                    "Failed to read from pipe for mime type {} : {}",
                                    writer.mime().mime(),
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
        fs,
        io::{PipeWriter, Write},
        path::PathBuf,
        sync::{
            atomic::{AtomicU32, Ordering},
            mpsc::{self, Receiver},
        },
        thread,
    };

    use clipleaf_core::MimeKind;

    use super::*;
    use crate::storage::Entry;

    const IDLE: Duration = Duration::from_secs(10); // only bounds a hang; never reached
    const ID: u64 = 7;

    /// A private payload dir, removed when the test ends.
    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "clipleafd-fetch-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        fn files(&self) -> Vec<(String, u64)> {
            let mut v: Vec<_> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    (
                        e.file_name().into_string().unwrap(),
                        e.metadata().unwrap().len(),
                    )
                })
                .collect();
            v.sort();
            v
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One pipe per type; the test plays the source on the write ends.
    fn sources(names: &[&'static str]) -> (Vec<PipeReader>, Vec<PipeWriter>) {
        names.iter().map(|_| io::pipe().unwrap()).unzip()
    }

    fn mimes(names: &[&'static str]) -> Vec<Mime<Cow<'static, str>>> {
        names
            .iter()
            .map(|n| {
                let kind = if n.starts_with("image/") {
                    MimeKind::Image
                } else {
                    MimeKind::Text
                };
                Mime::new(Cow::Borrowed(*n), kind, Some(0))
            })
            .collect()
    }

    /// Runs one fetch into `dir`; returns the outcome and the entry sent to the index, if any.
    fn run(
        dir: &Dir,
        readers: Vec<PipeReader>,
        names: &[&'static str],
        cancel: &EventFd,
        idle: Duration,
    ) -> (JobOutcome, Option<Entry>) {
        let (tx, rx): (Sender<IndexMsg>, Receiver<IndexMsg>) = mpsc::channel();
        let entry = EntryBuilder::new(ID, &dir.0);
        let outcome = fetch((readers, mimes(names)), entry, cancel, idle, tx, ID, 0);
        let sent = rx
            .try_recv()
            .ok()
            .map(|IndexMsg::Insert { entry, .. }| entry);
        (outcome, sent)
    }

    fn fetched(outcome: JobOutcome) -> Vec<(Cow<'static, str>, u64)> {
        match outcome {
            JobOutcome::Data(JobData::Fetch(types)) => types,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_type_is_read_to_its_end_in_fetch_order_and_only_large_types_get_a_file() {
        let dir = Dir::new();
        let (readers, mut writers) = sources(&["text/plain", "image/png"]);
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
        let (outcome, sent) = run(&dir, readers, &["text/plain", "image/png"], &cancel, IDLE);
        image.join().unwrap();
        assert_eq!(
            fetched(outcome),
            [("text/plain".into(), 5), ("image/png".into(), 300 * 1024)]
        );
        let entry = sent.expect("the entry goes to the index");
        let types: Vec<_> = entry
            .types()
            .iter()
            .map(|t| (t.mime().mime().to_string(), t.size()))
            .collect();
        assert_eq!(
            types,
            [
                ("text/plain".to_string(), 5),
                ("image/png".to_string(), 300 * 1024)
            ]
        );
        // The 5-byte text stays inline; the image (fetch position 1) is in <id>.1, complete.
        assert_eq!(dir.files(), [(format!("{ID}.1"), 300 * 1024)]);
        drop(entry); // never committed by an index: its files go
        assert!(dir.files().is_empty());
    }

    #[test]
    fn exactly_the_inline_limit_stays_inline_and_one_byte_more_gets_a_file() {
        for (len, file) in [(5 * 1024, false), (5 * 1024 + 1, true)] {
            let dir = Dir::new();
            let (readers, mut writers) = sources(&["text/plain"]);
            writers[0].write_all(&vec![b'x'; len]).unwrap();
            drop(writers);
            let cancel = EventFd::new().unwrap();
            let (outcome, sent) = run(&dir, readers, &["text/plain"], &cancel, IDLE);
            assert_eq!(fetched(outcome), [("text/plain".into(), len as u64)]);
            let expected: Vec<(String, u64)> = if file {
                vec![(format!("{ID}.0"), len as u64)]
            } else {
                vec![]
            };
            assert_eq!(dir.files(), expected, "{len} B");
            drop(sent);
        }
    }

    #[test]
    fn empty_types_are_left_out_and_all_empty_sends_nothing() {
        let dir = Dir::new();
        let (readers, mut writers) = sources(&["text/plain", "text/html"]);
        writers[1].write_all(b"<b>").unwrap();
        drop(writers);
        let cancel = EventFd::new().unwrap();
        let (outcome, sent) = run(&dir, readers, &["text/plain", "text/html"], &cancel, IDLE);
        assert_eq!(fetched(outcome), [("text/html".into(), 3)]);
        assert_eq!(sent.unwrap().types().len(), 1);

        let (readers, writers) = sources(&["text/plain", "text/html"]);
        drop(writers);
        let (outcome, sent) = run(&dir, readers, &["text/plain", "text/html"], &cancel, IDLE);
        assert!(fetched(outcome).is_empty());
        assert!(sent.is_none());
    }

    #[test]
    fn a_silent_source_times_out_and_the_finished_types_and_their_files_are_dropped_too() {
        // Never partial: one type that never ends loses the whole copy, including a file already written.
        let dir = Dir::new();
        let (readers, mut writers) = sources(&["text/plain", "image/png"]);
        writers[0].write_all(&vec![b'x'; 20 * 1024]).unwrap();
        drop(writers.remove(0));
        let _silent = writers; // stays open, never written

        let cancel = EventFd::new().unwrap();
        let (outcome, sent) = run(
            &dir,
            readers,
            &["text/plain", "image/png"],
            &cancel,
            Duration::from_millis(50),
        );
        assert!(matches!(outcome, JobOutcome::TimedOut), "{outcome:?}");
        assert!(sent.is_none());
        assert!(dir.files().is_empty(), "{:?}", dir.files());
    }

    #[test]
    fn a_cancelled_fetch_stops_without_waiting_and_leaves_no_file() {
        // The fetch has already written part of a file when the cancel arrives: that file goes too.
        let dir = Dir::new();
        let (readers, mut writers) = sources(&["image/png"]);
        writers[0].write_all(&vec![1u8; 16 * 1024]).unwrap(); // more than the inline limit, still open
        let cancel = EventFd::new().unwrap();
        let (outcome, sent) = thread::scope(|s| {
            s.spawn(|| {
                thread::sleep(Duration::from_millis(100)); // the 16 KiB are read long before this
                cancel.signal().unwrap();
            });
            run(&dir, readers, &["image/png"], &cancel, IDLE)
        });
        assert!(matches!(outcome, JobOutcome::Cancelled), "{outcome:?}");
        assert!(sent.is_none());
        assert!(dir.files().is_empty());
        drop(writers);
    }

    #[test]
    fn a_source_that_keeps_sending_is_never_cut_off_however_long_it_takes() {
        // The timeout counts silence, not the whole fetch: 600 ms of steady 20 ms gaps against a
        // 300 ms idle timeout. Only a gap 15x the planned one could fail this.
        let dir = Dir::new();
        let (readers, mut writers) = sources(&["text/plain"]);
        let mut slow = writers.pop().unwrap();
        let source = thread::spawn(move || {
            for _ in 0..30 {
                slow.write_all(b"x").unwrap();
                thread::sleep(Duration::from_millis(20));
            }
        });
        let cancel = EventFd::new().unwrap();
        let (outcome, _sent) = run(
            &dir,
            readers,
            &["text/plain"],
            &cancel,
            Duration::from_millis(300),
        );
        source.join().unwrap();
        assert_eq!(fetched(outcome), [("text/plain".into(), 30)]);
    }
}
