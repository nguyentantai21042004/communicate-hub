//! One queue per topic, with a cap on how many topics run at once.
//!
//! A topic has a single worker, so its jobs run in submission order. Different topics take
//! permits from one semaphore. A failed job does not stick the lane: the worker always moves
//! on to the next job.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Notify, Semaphore, SemaphorePermit, oneshot};

use crate::error::{Error, Result};

pub(crate) struct Job<T> {
    payload: Option<T>,
    done: Option<oneshot::Sender<Result<()>>>,
}

impl<T> Job<T> {
    pub(crate) fn take_payload(&mut self) -> Option<T> {
        self.payload.take()
    }

    pub(crate) fn finish(mut self, result: Result<()>) {
        if let Some(done) = self.done.take() {
            let _ = done.send(result);
        }
    }
}

struct Lane<T> {
    jobs: VecDeque<Job<T>>,
    running: bool,
}

impl<T> Default for Lane<T> {
    fn default() -> Self {
        Self {
            jobs: VecDeque::new(),
            running: false,
        }
    }
}

pub(crate) struct LaneQueue<K, T> {
    lanes: Mutex<HashMap<K, Lane<T>>>,
    sem: Semaphore,
    depth: AtomicUsize,
    running: AtomicUsize,
    idle: Notify,
}

impl<K, T> LaneQueue<K, T>
where
    K: Eq + std::hash::Hash + Clone,
{
    pub(crate) fn new(concurrency: usize) -> Self {
        Self {
            lanes: Mutex::new(HashMap::new()),
            sem: Semaphore::new(concurrency.max(1)),
            depth: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            idle: Notify::new(),
        }
    }

    /// Queues `payload`. The bool is true when the caller must start a worker for `key`.
    pub(crate) fn push(&self, key: K, payload: T) -> (oneshot::Receiver<Result<()>>, bool) {
        let (tx, rx) = oneshot::channel();
        let mut lanes = lock(&self.lanes);
        let lane = lanes.entry(key).or_default();
        lane.jobs.push_back(Job {
            payload: Some(payload),
            done: Some(tx),
        });
        self.depth.fetch_add(1, Ordering::SeqCst);
        let start = !lane.running;
        lane.running = true;
        (rx, start)
    }

    pub(crate) fn pop(&self, key: &K) -> Option<Job<T>> {
        let mut lanes = lock(&self.lanes);
        let lane = lanes.get_mut(key)?;
        if let Some(job) = lane.jobs.pop_front() {
            return Some(job);
        }
        lanes.remove(key);
        None
    }

    pub(crate) async fn acquire(&self) -> std::result::Result<SemaphorePermit<'_>, ()> {
        self.sem.acquire().await.map_err(|_closed| ())
    }

    pub(crate) fn finish_one(&self) {
        self.depth.fetch_sub(1, Ordering::SeqCst);
        self.idle.notify_waiters();
    }

    pub(crate) fn worker_started(&self) {
        self.running.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn worker_finished(&self) {
        self.running.fetch_sub(1, Ordering::SeqCst);
        self.idle.notify_waiters();
    }

    /// Fails every job still queued for `key` and forgets the lane.
    pub(crate) fn abandon(&self, key: &K) {
        let jobs = lock(&self.lanes)
            .remove(key)
            .map(|lane| lane.jobs)
            .unwrap_or_default();
        for job in jobs {
            job.finish(Err(Error::ShutDown));
            self.finish_one();
        }
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            if self.is_idle() {
                return;
            }
            notified.as_mut().enable();
            if self.is_idle() {
                return;
            }
            notified.await;
        }
    }

    fn is_idle(&self) -> bool {
        self.depth.load(Ordering::SeqCst) == 0 && self.running.load(Ordering::SeqCst) == 0
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
