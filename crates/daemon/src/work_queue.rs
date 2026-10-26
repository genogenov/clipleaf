use std::{
    collections::VecDeque,
    sync::{Condvar, Mutex, PoisonError},
};

pub struct WorkQueue<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}
struct State<T> {
    jobs: VecDeque<T>,
    closed: bool,
}
impl<T> WorkQueue<T> {
    pub fn new() -> Self {
        WorkQueue {
            state: Mutex::new(State {
                jobs: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    pub fn push(&self, job: T) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .jobs
            .push_back(job);
        self.ready.notify_one();
    }

    pub fn try_push(&self, job: T, if_less_than: usize) -> Option<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);

        if state.jobs.len() >= if_less_than {
            return None;
        }

        state.jobs.push_back(job);
        self.ready.notify_one();
        Some(())
    }

    fn pop(&self, take: impl FnOnce(&mut VecDeque<T>) -> Option<T>) -> Option<T> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.jobs.is_empty() && !state.closed {
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if state.closed {
            None
        } else {
            take(&mut state.jobs)
        }
    }
    pub fn pop_newest(&self) -> Option<T> {
        self.pop(VecDeque::pop_back)
    }
    pub fn pop_oldest(&self) -> Option<T> {
        self.pop(VecDeque::pop_front)
    }

    // pub fn clear(&self) -> usize {
    //     let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
    //     let len = state.jobs.len();
    //     state.jobs.clear();
    //     len
    // }

    pub fn retain<F: FnMut(&T) -> bool>(&self, f: F) -> usize {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let len = state.jobs.len();
        state.jobs.retain(f);
        len
    }

    // pub fn close(&self) {
    //     self.state
    //         .lock()
    //         .unwrap_or_else(PoisonError::into_inner)
    //         .closed = true;
    //     self.ready.notify_all();
    // }
}
