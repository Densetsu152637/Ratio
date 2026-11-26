//! The bounded blocking pool large pure built-ins run on (chapter 01; chapter 07, "Resource
//! limits"; design PC-6).
//!
//! A [`Pool`] owns a fixed set of worker threads; the host's evaluation driver creates one and
//! hands each session a [`PoolSession`]. A pure built-in without callbacks whose input passes a
//! [`Thresholds`] (64 KiB of text or 16,384 elements by default) runs on a worker while its
//! evaluation suspends ([`crate::Suspend::Wait`]), so the runtime thread is free for other
//! sessions; the result, labels and charges are those it would have had inline. No program
//! code ever runs here: only the bodies of built-ins.
//!
//! Each session has at most [`JOBS_IN_FLIGHT`] jobs in flight; further offloads wait in the
//! session's queue, in order. A job keeps its place until it **finishes**, even when the branch
//! that issued it was cancelled and stopped waiting: its steps were debited and its heap
//! charged before dispatch, and both stay spent until it ends, so cancelling and re-issuing
//! cannot multiply a session's use of the pool. Dropping the pool runs every job already
//! queued, then joins every worker; a job submitted after that runs on the submitting thread.

use std::collections::VecDeque;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::Waker;
use std::thread::JoinHandle;

/// Offloaded jobs one session may have in flight (chapter 07: an implementation constant, 2).
pub const JOBS_IN_FLIGHT: usize = 2;

/// Text at least this long (in bytes) makes a built-in's input large.
pub const OFFLOAD_TEXT_BYTES: u64 = 64 * 1024;

/// A list, map or JSON value with at least this many elements makes a built-in's input large.
pub const OFFLOAD_ELEMENTS: u64 = 16_384;

/// When a built-in's input counts as large enough to offload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// Bytes of any one text argument.
    pub text_bytes: u64,
    /// Elements (or JSON nodes) of any one container argument.
    pub elements: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            text_bytes: OFFLOAD_TEXT_BYTES,
            elements: OFFLOAD_ELEMENTS,
        }
    }
}

type Job = Box<dyn FnOnce() + Send>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shutdown: bool,
}

#[derive(Default)]
struct Inner {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Inner {
    /// Queues a job for a worker, or runs it here once the pool has shut down.
    fn push(&self, job: Job) {
        let mut q = lock(&self.queue);
        if q.shutdown {
            drop(q);
            job();
            return;
        }
        q.jobs.push_back(job);
        drop(q);
        self.ready.notify_one();
    }

    fn work(&self) {
        loop {
            let job = {
                let mut q = lock(&self.queue);
                loop {
                    if let Some(job) = q.jobs.pop_front() {
                        break Some(job);
                    }
                    if q.shutdown {
                        break None;
                    }
                    q = self.ready.wait(q).unwrap_or_else(PoisonError::into_inner);
                }
            };
            match job {
                Some(job) => job(),
                None => return,
            }
        }
    }
}

/// A bounded pool of worker threads for large pure built-ins.
pub struct Pool {
    inner: Arc<Inner>,
    workers: Vec<JoinHandle<()>>,
    thresholds: Thresholds,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("workers", &self.workers.len())
            .field("thresholds", &self.thresholds)
            .finish_non_exhaustive()
    }
}

impl Pool {
    /// A pool of `threads` workers (at least one) with the default thresholds.
    #[must_use]
    pub fn new(threads: usize) -> Pool {
        Pool::with_thresholds(threads, Thresholds::default())
    }

    /// A pool whose sessions offload inputs past `thresholds`.
    #[must_use]
    pub fn with_thresholds(threads: usize, thresholds: Thresholds) -> Pool {
        let inner = Arc::new(Inner::default());
        let workers = (0..threads.max(1))
            .filter_map(|i| {
                let inner = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name(format!("ratio-pool-{i}"))
                    .stack_size(8 * 1024 * 1024)
                    .spawn(move || inner.work())
                    .ok()
            })
            .collect::<Vec<_>>();
        if workers.is_empty() {
            // No thread could start: jobs run where they are submitted.
            lock(&inner.queue).shutdown = true;
        }
        Pool {
            inner,
            workers,
            thresholds,
        }
    }

    /// A new session's handle: its own in-flight cap, sharing the pool's workers.
    #[must_use]
    pub fn session(&self) -> PoolSession {
        PoolSession {
            pool: Arc::clone(&self.inner),
            state: Arc::new(Mutex::new(SessionState::default())),
            thresholds: self.thresholds,
        }
    }

    /// The pool's worker threads.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        lock(&self.inner.queue).shutdown = true;
        self.inner.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct SessionState {
    in_flight: usize,
    waiting: VecDeque<Job>,
}

/// One session's use of a [`Pool`]: at most [`JOBS_IN_FLIGHT`] jobs in flight. Clones share
/// the cap; give every evaluation of a session the same one.
#[derive(Clone)]
pub struct PoolSession {
    pool: Arc<Inner>,
    state: Arc<Mutex<SessionState>>,
    thresholds: Thresholds,
}

impl fmt::Debug for PoolSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolSession")
            .field("in_flight", &self.in_flight())
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

impl PoolSession {
    /// Jobs dispatched to the workers and not yet finished (cancelled ones included).
    #[must_use]
    pub fn in_flight(&self) -> usize {
        lock(&self.state).in_flight
    }

    /// Jobs waiting for a place under the cap.
    #[must_use]
    pub fn queued(&self) -> usize {
        lock(&self.state).waiting.len()
    }

    pub(crate) fn thresholds(&self) -> Thresholds {
        self.thresholds
    }

    /// Runs `f` on the pool under the session's cap; the handle gives its result.
    pub(crate) fn submit<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> JobHandle<T> {
        let slot = Arc::new(Slot::default());
        let done = Arc::clone(&slot);
        let job: Job = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(f)).map_err(|_| Panicked);
            done.fill(result);
        });
        self.dispatch(job);
        JobHandle { slot }
    }

    fn dispatch(&self, job: Job) {
        let mut s = lock(&self.state);
        if s.in_flight < JOBS_IN_FLIGHT {
            s.in_flight += 1;
            drop(s);
            self.pool.push(self.wrap(job));
        } else {
            s.waiting.push_back(job);
        }
    }

    /// The job, then its release of the session's place (and the next waiting job's start).
    fn wrap(&self, job: Job) -> Job {
        let session = self.clone();
        Box::new(move || {
            // Everything the job holds (its arguments, its heap charge, its result if nobody
            // waits for it any more) is dropped here, before its place is given back.
            job();
            session.finish();
        })
    }

    fn finish(&self) {
        let next = {
            let mut s = lock(&self.state);
            s.in_flight -= 1;
            let next = s.waiting.pop_front();
            if next.is_some() {
                s.in_flight += 1;
            }
            next
        };
        if let Some(job) = next {
            self.pool.push(self.wrap(job));
        }
    }
}

/// A job's body panicked.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Panicked;

struct Slot<T> {
    state: Mutex<SlotState<T>>,
    done: Condvar,
}

struct SlotState<T> {
    result: Option<Result<T, Panicked>>,
    finished: bool,
    waker: Option<Waker>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            state: Mutex::new(SlotState {
                result: None,
                finished: false,
                waker: None,
            }),
            done: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    fn fill(&self, result: Result<T, Panicked>) {
        let waker = {
            let mut s = lock(&self.state);
            s.result = Some(result);
            s.finished = true;
            s.waker.take()
        };
        self.done.notify_all();
        if let Some(w) = waker {
            w.wake();
        }
    }
}

trait Waitable: Send + Sync {
    fn register(&self, waker: &Waker);
    fn ready(&self) -> bool;
    fn wait(&self);
}

impl<T: Send> Waitable for Slot<T> {
    fn register(&self, waker: &Waker) {
        let mut s = lock(&self.state);
        if !s.finished {
            s.waker = Some(waker.clone());
        }
    }

    fn ready(&self) -> bool {
        lock(&self.state).finished
    }

    fn wait(&self) {
        let mut s = lock(&self.state);
        while !s.finished {
            s = self.done.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// The result of a job on the pool, when it is ready.
pub(crate) struct JobHandle<T> {
    slot: Arc<Slot<T>>,
}

impl<T> fmt::Debug for JobHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JobHandle")
    }
}

impl<T: Send + 'static> JobHandle<T> {
    /// The result, once the job has finished (taken once).
    pub(crate) fn try_take(&self) -> Option<Result<T, Panicked>> {
        let mut s = lock(&self.slot.state);
        if s.finished { s.result.take() } else { None }
    }

    pub(crate) fn waiter(&self) -> Waiter {
        Waiter(Arc::clone(&self.slot) as Arc<dyn Waitable>)
    }
}

/// What an evaluation suspended at [`crate::Suspend::Wait`] waits for: a built-in running on
/// the pool. Register a waker (or block with [`Waiter::wait`]), then call
/// [`crate::Machine::run`] again.
#[derive(Clone)]
pub struct Waiter(Arc<dyn Waitable>);

impl fmt::Debug for Waiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Waiter")
            .field("ready", &self.is_ready())
            .finish()
    }
}

impl Waiter {
    /// Wakes `waker` when the job finishes (at once if it has).
    pub fn register(&self, waker: &Waker) {
        self.0.register(waker);
        if self.0.ready() {
            waker.wake_by_ref();
        }
    }

    /// Whether the job has finished.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.0.ready()
    }

    /// Blocks the current thread until the job finishes.
    pub fn wait(&self) {
        self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_session_has_at_most_two_jobs_in_flight() {
        let pool = Pool::new(4);
        let session = pool.session();
        let (go, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let mut handles = Vec::new();
        for i in 0..4 {
            let gate = Arc::clone(&gate);
            handles.push(session.submit(move || {
                let _ = lock(&gate).recv();
                i
            }));
        }
        assert_eq!(session.in_flight(), 2);
        assert_eq!(session.queued(), 2);
        for _ in 0..4 {
            go.send(()).unwrap();
        }
        let got: Vec<i32> = handles
            .iter()
            .map(|h| {
                h.waiter().wait();
                h.try_take().unwrap().unwrap()
            })
            .collect();
        assert_eq!(got, [0, 1, 2, 3]);
        drop(pool);
        assert_eq!(session.in_flight(), 0);
    }

    #[test]
    fn dropping_the_pool_runs_queued_jobs_and_joins_its_workers() {
        let pool = Pool::new(1);
        let session = pool.session();
        let handles: Vec<_> = (0..5).map(|i| session.submit(move || i * 2)).collect();
        drop(pool);
        for (i, h) in handles.iter().enumerate() {
            assert!(h.waiter().is_ready());
            assert_eq!(h.try_take().unwrap().unwrap(), i * 2);
        }
        // After shutdown a job runs where it is submitted.
        let late = session.submit(|| 7);
        assert_eq!(late.try_take().unwrap().unwrap(), 7);
    }

    #[test]
    fn a_panicking_job_is_reported_and_frees_its_place() {
        let pool = Pool::new(1);
        let session = pool.session();
        let h = session.submit(|| -> u8 { panic!("boom") });
        h.waiter().wait();
        assert!(h.try_take().unwrap().is_err());
        let ok = session.submit(|| 1u8);
        ok.waiter().wait();
        assert_eq!(ok.try_take().unwrap().unwrap(), 1);
    }
}
