//! In-flight cap and the global pause after HTTP 429.
//!
//! A 429 sets one pause for every caller. The call that hit it is retried
//! after `retry_after` plus [`crate::limits::RETRY_BACKOFF_EXTRA_SECS`].
//! After [`crate::limits::MAX_RATE_LIMIT_RETRIES`] retries the error is returned.

use std::future::Future;
use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, Instant};

use crate::error::{self, Error, Result};
use crate::limits::{MAX_RATE_LIMIT_RETRIES, REQUEST_CONCURRENCY, RETRY_BACKOFF_EXTRA_SECS};

struct Pause {
    until: Instant,
}

/// Shared gate in front of the Bot API.
#[derive(Clone)]
pub(crate) struct RequestQueue {
    slots: Arc<Semaphore>,
    pause: Arc<Mutex<Pause>>,
}

impl RequestQueue {
    pub(crate) fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(REQUEST_CONCURRENCY)),
            pause: Arc::new(Mutex::new(Pause {
                until: Instant::now(),
            })),
        }
    }

    /// Runs `op`, waiting for a free send slot and for any 429 pause.
    ///
    /// `op` is called again after a 429 until the retry budget is spent.
    /// The future from one call is awaited before the next call, so `op`
    /// may capture a reference for a single attempt.
    pub(crate) async fn run<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = std::result::Result<T, frankenstein::Error>> + Send,
    {
        self.execute(op, true).await
    }

    /// Runs `op` outside the send-slot cap.
    ///
    /// `getUpdates` holds its request for up to 25 seconds. It still waits for a 429 pause
    /// and still extends that pause, so a flood from the long poll stops sends too.
    pub(crate) async fn run_polling<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = std::result::Result<T, frankenstein::Error>> + Send,
    {
        self.execute(op, false).await
    }

    async fn execute<T, F, Fut>(&self, mut op: F, limited: bool) -> Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = std::result::Result<T, frankenstein::Error>> + Send,
    {
        let mut retries = 0;
        loop {
            self.wait_until_open().await;
            let permit = if limited {
                Some(
                    self.slots
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|_| Error::Permanent { status: None })?,
                )
            } else {
                None
            };
            // The pause may have started while this call waited for a slot.
            self.wait_until_open().await;
            let attempt = op().await;
            drop(permit);
            match attempt {
                Ok(value) => return Ok(value),
                Err(err) => {
                    if let Some(after_secs) = error::retry_after_secs(&err) {
                        if retries < MAX_RATE_LIMIT_RETRIES {
                            self.extend_pause(after_secs + RETRY_BACKOFF_EXTRA_SECS)
                                .await;
                            retries += 1;
                            continue;
                        }
                        return Err(Error::RateLimited {
                            retry_after_secs: after_secs,
                        });
                    }
                    return Err(error::map_frankenstein(err));
                }
            }
        }
    }

    async fn wait_until_open(&self) {
        loop {
            let until = self.pause.lock().await.until;
            if Instant::now() >= until {
                return;
            }
            tokio::time::sleep_until(until).await;
        }
    }

    async fn extend_pause(&self, wait_secs: u64) {
        let mut pause = self.pause.lock().await;
        let until = Instant::now() + Duration::from_secs(wait_secs);
        if until > pause.until {
            pause.until = until;
        }
    }
}
