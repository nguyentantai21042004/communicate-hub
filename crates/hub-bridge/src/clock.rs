//! Time source for the bridge.
//!
//! Tests pass [`ManualClock`] and advance it. Production passes [`SystemClock`]. Nothing in
//! this crate calls [`std::thread::sleep`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{SystemTime, UNIX_EPOCH};

/// A clock the bridge can read and wait on.
pub trait Clock: Send + Sync {
    /// Current time as Unix seconds.
    fn unix_secs(&self) -> i64;

    /// Resolves when [`unix_secs`](Self::unix_secs) is at least `deadline_unix_secs`.
    ///
    /// A deadline that is already past resolves immediately. Dropping the future cancels the wait.
    fn sleep_until(&self, deadline_unix_secs: i64) -> impl Future<Output = ()> + Send;
}

/// Wall clock backed by `tokio`'s timer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
            })
    }

    async fn sleep_until(&self, deadline_unix_secs: i64) {
        let now = self.unix_secs();
        if deadline_unix_secs <= now {
            return;
        }
        let secs = u64::try_from(deadline_unix_secs - now).unwrap_or(0);
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
    }
}

/// Clock whose time moves only when a test calls [`ManualClock::advance`].
#[derive(Debug, Clone)]
pub struct ManualClock {
    inner: Arc<ManualInner>,
}

#[derive(Debug)]
struct ManualInner {
    now_secs: Mutex<i64>,
    waiters: Mutex<Vec<Waiter>>,
    next_id: std::sync::atomic::AtomicU64,
}

#[derive(Debug)]
struct Waiter {
    id: u64,
    deadline_unix_secs: i64,
    waker: Waker,
}

impl ManualClock {
    /// A clock frozen at `start_unix_secs`.
    pub fn new(start_unix_secs: i64) -> Self {
        Self {
            inner: Arc::new(ManualInner {
                now_secs: Mutex::new(start_unix_secs),
                waiters: Mutex::new(Vec::new()),
                next_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    /// Moves time forward by `secs` and wakes waiters whose deadline has arrived.
    pub fn advance(&self, secs: i64) {
        let now = {
            let mut now = lock(&self.inner.now_secs);
            *now = now.saturating_add(secs);
            *now
        };
        self.wake_due(now);
    }

    fn wake_due(&self, now: i64) {
        let due = {
            let mut waiters = lock(&self.inner.waiters);
            let mut due = Vec::new();
            let mut left = Vec::new();
            for waiter in waiters.drain(..) {
                if waiter.deadline_unix_secs <= now {
                    due.push(waiter);
                } else {
                    left.push(waiter);
                }
            }
            *waiters = left;
            due
        };
        for waiter in due {
            waiter.waker.wake();
        }
    }
}

impl Clock for ManualClock {
    fn unix_secs(&self) -> i64 {
        *lock(&self.inner.now_secs)
    }

    fn sleep_until(&self, deadline_unix_secs: i64) -> impl Future<Output = ()> + Send {
        Sleep {
            inner: Arc::clone(&self.inner),
            id: self
                .inner
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            deadline_unix_secs,
            armed: false,
        }
    }
}

struct Sleep {
    inner: Arc<ManualInner>,
    id: u64,
    deadline_unix_secs: i64,
    armed: bool,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if *lock(&this.inner.now_secs) >= this.deadline_unix_secs {
            this.disarm();
            return Poll::Ready(());
        }
        {
            let mut waiters = lock(&this.inner.waiters);
            if let Some(waiter) = waiters.iter_mut().find(|waiter| waiter.id == this.id) {
                waiter.waker = context.waker().clone();
            } else {
                waiters.push(Waiter {
                    id: this.id,
                    deadline_unix_secs: this.deadline_unix_secs,
                    waker: context.waker().clone(),
                });
            }
            this.armed = true;
        }
        // `advance` can land between the first time check and registration. Recheck so that
        // wait is not lost.
        if *lock(&this.inner.now_secs) >= this.deadline_unix_secs {
            this.disarm();
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

impl Sleep {
    fn disarm(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        lock(&self.inner.waiters).retain(|waiter| waiter.id != self.id);
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        self.disarm();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panicked waiter must not freeze every other wait on this clock.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
