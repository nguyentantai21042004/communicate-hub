//! The bridge runtime: startup, per-topic queue, rate-limit gate, and health.
//!
//! Delivery rules live in [`deliver`]. This file owns task lifetime. `run` restores mappings
//! before it subscribes to adapters, and on shutdown it stops readers and then drains the queue.

mod deliver;

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hub_core::{
    AttachmentSource, ConversationId, ModuleHealth, ModuleState, RemoteEvent, TgMessageId, TopicId,
};
use tokio::sync::{oneshot, watch};

use crate::adapter::AdapterHandle;
use crate::clock::Clock;
use crate::config::BridgeConfig;
use crate::error::{Error, Result};
use crate::incoming::TopicIncoming;
use crate::limits::{MODULE_NAME, RETRY_WINDOW_SECS, TOPIC_CONCURRENCY};
use crate::queue::LaneQueue;
use crate::store::{BridgeStore, FailureRecord, TopicMapping, clip_reason};

const LIFE_IDLE: u8 = 0;
const LIFE_RUNNING: u8 = 1;
const LIFE_STOPPED: u8 = 2;
const OPS_LANE: &str = "ops";

pub(crate) struct Cache {
    topics: HashMap<ConversationId, TopicId>,
    conversations: HashMap<TopicId, ConversationId>,
    titles: HashMap<ConversationId, String>,
    applied: HashMap<ConversationId, String>,
    renamed_at: HashMap<ConversationId, i64>,
    avatars: HashMap<ConversationId, AttachmentSource>,
}

impl Cache {
    fn new() -> Self {
        Self {
            topics: HashMap::new(),
            conversations: HashMap::new(),
            titles: HashMap::new(),
            applied: HashMap::new(),
            renamed_at: HashMap::new(),
            avatars: HashMap::new(),
        }
    }
}

pub(crate) enum Work {
    Remote(RemoteEvent),
    Incoming(TopicIncoming),
}

#[derive(Clone)]
pub(crate) enum Plan {
    Text {
        html: String,
        reply: Option<TgMessageId>,
    },
    Media {
        attachments: Vec<hub_core::Attachment>,
        caption: Option<String>,
    },
}

#[derive(Clone)]
pub(crate) enum SinkOp {
    Create(String),
    Text {
        topic: TopicId,
        html: String,
        reply: Option<TgMessageId>,
    },
    Media {
        topic: TopicId,
        attachment: hub_core::Attachment,
        caption: Option<String>,
    },
    React {
        topic: TopicId,
        tg: TgMessageId,
        emoji: Option<String>,
    },
    Delete {
        topic: TopicId,
        tg: TgMessageId,
    },
    Rename {
        topic: TopicId,
        title: String,
    },
}

#[derive(Clone)]
pub(crate) enum SinkOut {
    Topic(TopicId),
    Message(TgMessageId),
    Unit,
}

pub(crate) enum Step<T> {
    Done(T),
    Sink(hub_core::Error),
}

pub(crate) struct Inner<S, St, C> {
    config: BridgeConfig,
    adapters: Vec<Box<dyn AdapterHandle>>,
    sink: S,
    store: St,
    clock: C,
    queue: LaneQueue<String, Work>,
    cache: Mutex<Cache>,
    rate_until: Mutex<Option<i64>>,
    retries: Mutex<Vec<i64>>,
    open_failures: AtomicU64,
    life: AtomicU8,
    stop: watch::Sender<bool>,
}

/// Routes [`RemoteEvent`]s into forum topics and topic replies back to the source network.
///
/// `S` is the Telegram side, `St` is the mapping store, and `C` is the clock. Adapters are
/// trait objects because one bridge serves every network at once.
pub struct Bridge<S, St, C> {
    inner: Arc<Inner<S, St, C>>,
}

impl<S, St, C> Clone for Bridge<S, St, C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<S, St, C> Bridge<S, St, C>
where
    S: hub_core::TopicSink + 'static,
    St: BridgeStore + 'static,
    C: Clock + 'static,
{
    /// Builds a bridge that has not subscribed to `adapters` yet.
    ///
    /// [`Self::run`] is what restores mappings and then starts reading. [`Self::handle_remote`]
    /// and [`Self::handle_topic_message`] work before `run` so tests can drive one event.
    pub fn new(
        config: BridgeConfig,
        adapters: Vec<Box<dyn AdapterHandle>>,
        sink: S,
        store: St,
        clock: C,
    ) -> Self {
        let (stop, _rx) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                config,
                adapters,
                sink,
                store,
                clock,
                queue: LaneQueue::new(TOPIC_CONCURRENCY),
                cache: Mutex::new(Cache::new()),
                rate_until: Mutex::new(None),
                retries: Mutex::new(Vec::new()),
                open_failures: AtomicU64::new(0),
                life: AtomicU8::new(LIFE_IDLE),
                stop,
            }),
        }
    }

    /// Restores mappings, subscribes to adapters, and returns after `shutdown` once the queue drains.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyRunning`] or [`Error::ShutDown`] when `run` is not in the idle
    /// state, and [`Error::Store`] when restoring mappings fails. Adapter read errors are
    /// recorded and do not fail this call.
    pub async fn run(&self, shutdown: impl Future<Output = ()> + Send) -> Result<()> {
        match self.inner.life.compare_exchange(
            LIFE_IDLE,
            LIFE_RUNNING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => {}
            Err(LIFE_RUNNING) => return Err(Error::AlreadyRunning),
            Err(_) => return Err(Error::ShutDown),
        }
        if let Err(error) = self.inner.restore().await {
            self.inner.life.store(LIFE_IDLE, Ordering::SeqCst);
            return Err(error);
        }
        let mut readers = Vec::with_capacity(self.inner.adapters.len());
        for index in 0..self.inner.adapters.len() {
            let inner = Arc::clone(&self.inner);
            readers.push(tokio::spawn(async move {
                inner.read_adapter(index).await;
            }));
        }
        shutdown.await;
        let _ = self.inner.stop.send(true);
        for reader in readers {
            let _ = reader.await;
        }
        self.inner.queue.wait_idle().await;
        self.inner.life.store(LIFE_STOPPED, Ordering::SeqCst);
        Ok(())
    }

    /// Delivers one source event.
    ///
    /// A delivery failure is stored and this returns `Ok`. The returned error means the event
    /// was not queued, or the store itself failed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ShutDown`] after [`Self::run`] has finished, and [`Error::Store`] when
    /// a mapping write fails.
    pub async fn handle_remote(&self, event: RemoteEvent) -> Result<()> {
        self.inner.handle_remote(event).await
    }

    /// Delivers one Telegram topic update to the source network.
    ///
    /// # Errors
    ///
    /// Same as [`Self::handle_remote`]. An unknown topic is recorded as a non-retryable failure
    /// and this still returns `Ok`.
    pub async fn handle_topic_message(&self, incoming: TopicIncoming) -> Result<()> {
        self.inner.handle_incoming(incoming).await
    }

    /// Snapshot for module `bridge`: queue depth, retries in the last 24 hours, rate-limit state.
    pub fn health(&self) -> ModuleHealth {
        self.inner.health()
    }
}

impl<S, St, C> Inner<S, St, C>
where
    S: hub_core::TopicSink + 'static,
    St: BridgeStore + 'static,
    C: Clock + 'static,
{
    async fn restore(&self) -> Result<()> {
        let topics = self.store.load_topics().await?;
        {
            let mut cache = lock(&self.cache);
            for mapping in topics {
                cache_insert(&mut cache, &mapping);
            }
        }
        let since = self.clock.unix_secs().saturating_sub(RETRY_WINDOW_SECS);
        let stored = self.store.retry_marks_since(since).await?;
        let mut retries = lock(&self.retries);
        for at in stored {
            if !retries.contains(&at) {
                retries.push(at);
            }
        }
        Ok(())
    }

    async fn read_adapter(self: &Arc<Self>, index: usize) {
        let mut stop = self.stop.subscribe();
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                biased;
                event = self.adapters[index].next_event() => {
                    match event {
                        Ok(Some(event)) => {
                            if let Err(error) = self.enqueue_detached(event) {
                                tracing::warn!(%error, "remote event was not queued");
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(%error, "adapter read failed");
                            let _ = self.record_core(None, &error).await;
                            if !self.backoff(&mut stop).await {
                                break;
                            }
                        }
                    }
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
            }
        }
    }

    async fn backoff(&self, stop: &mut watch::Receiver<bool>) -> bool {
        let deadline = self
            .clock
            .unix_secs()
            .saturating_add(crate::limits::ADAPTER_BACKOFF_SECS);
        let sleep = self.clock.sleep_until(deadline);
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            _ = stop.changed() => false,
            () = &mut sleep => true,
        }
    }

    async fn handle_remote(self: &Arc<Self>, event: RemoteEvent) -> Result<()> {
        if self.is_stopped() {
            return Err(Error::ShutDown);
        }
        let Some(key) = lane_of_remote(&event) else {
            return self.dispatch_remote(event).await;
        };
        self.drive(key, Work::Remote(event)).await
    }

    async fn handle_incoming(self: &Arc<Self>, incoming: TopicIncoming) -> Result<()> {
        if self.is_stopped() {
            return Err(Error::ShutDown);
        }
        let topic = topic_of(&incoming);
        let Some(conversation) = self.conversation_for(topic).await? else {
            // An unknown topic is not a sink failure. Record it and keep the caller unblocked.
            self.store
                .record_failure(FailureRecord {
                    conversation: None,
                    reason: clip_reason(&format!("unmapped topic {}", topic.0)),
                    retryable: false,
                    created_at_unix_secs: self.clock.unix_secs(),
                })
                .await?;
            self.open_failures.fetch_add(1, Ordering::SeqCst);
            return Ok(());
        };
        self.drive(conversation.to_string(), Work::Incoming(incoming))
            .await
    }

    fn enqueue_detached(self: &Arc<Self>, event: RemoteEvent) -> Result<()> {
        if self.is_stopped() {
            return Err(Error::ShutDown);
        }
        let Some(key) = lane_of_remote(&event) else {
            let inner = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(error) = inner.dispatch_remote(event).await {
                    tracing::warn!(%error, "session event failed");
                }
            });
            return Ok(());
        };
        let rx = self.start(key, Work::Remote(event));
        drop(rx);
        Ok(())
    }

    async fn drive(self: &Arc<Self>, key: String, work: Work) -> Result<()> {
        let rx = self.start(key, work);
        rx.await.map_err(|_| Error::ShutDown)?
    }

    fn start(self: &Arc<Self>, key: String, work: Work) -> oneshot::Receiver<Result<()>> {
        let (rx, spawn) = self.queue.push(key.clone(), work);
        if spawn {
            let inner = Arc::clone(self);
            tokio::spawn(async move {
                inner.pump(key).await;
            });
        }
        rx
    }

    async fn pump(self: Arc<Self>, key: String) {
        self.queue.worker_started();
        let permit = match self.queue.acquire().await {
            Ok(permit) => permit,
            Err(()) => {
                self.queue.abandon(&key);
                self.queue.worker_finished();
                return;
            }
        };
        loop {
            let Some(mut job) = self.queue.pop(&key) else {
                break;
            };
            let Some(payload) = job.take_payload() else {
                job.finish(Ok(()));
                self.queue.finish_one();
                continue;
            };
            let result = self.execute(payload).await;
            job.finish(result);
            self.queue.finish_one();
        }
        drop(permit);
        self.queue.worker_finished();
    }

    async fn execute(&self, work: Work) -> Result<()> {
        match work {
            Work::Remote(event) => self.dispatch_remote(event).await,
            Work::Incoming(incoming) => self.dispatch_incoming(incoming).await,
        }
    }

    fn health(&self) -> ModuleHealth {
        let now = self.clock.unix_secs();
        let depth = self.queue.depth();
        let retries = self.retries_in_window(now);
        let remaining = self.rate_remaining(now);
        let detail = match remaining {
            Some(secs) => format!(
                "queue_depth={depth} retries_24h={retries} rate_limited=true retry_after_secs={secs}"
            ),
            None => format!("queue_depth={depth} retries_24h={retries} rate_limited=false"),
        };
        let state = if self.life.load(Ordering::SeqCst) == LIFE_STOPPED {
            ModuleState::Down
        } else if remaining.is_some() || self.open_failures.load(Ordering::SeqCst) > 0 {
            ModuleState::Degraded
        } else if self.life.load(Ordering::SeqCst) == LIFE_IDLE {
            ModuleState::Unknown
        } else {
            ModuleState::Ok
        };
        ModuleHealth {
            module: MODULE_NAME.to_owned(),
            state,
            detail,
            updated_at_unix_secs: now,
        }
    }

    fn retries_in_window(&self, now: i64) -> usize {
        let cutoff = now.saturating_sub(RETRY_WINDOW_SECS);
        lock(&self.retries)
            .iter()
            .filter(|at| **at >= cutoff)
            .count()
    }

    fn rate_remaining(&self, now: i64) -> Option<u64> {
        let until = (*lock(&self.rate_until))?;
        if until <= now {
            None
        } else {
            u64::try_from(until - now).ok()
        }
    }

    fn is_stopped(&self) -> bool {
        self.life.load(Ordering::SeqCst) == LIFE_STOPPED || *self.stop.borrow()
    }

    pub(crate) fn adapter_for(&self, network: hub_core::Network) -> Option<&dyn AdapterHandle> {
        self.adapters
            .iter()
            .find(|adapter| adapter.network() == network)
            .map(AsRef::as_ref)
    }

    pub(crate) fn pause(&self, retry_after_secs: u64) {
        let extra = i64::try_from(retry_after_secs).unwrap_or(i64::MAX);
        let until = self.clock.unix_secs().saturating_add(extra);
        let mut slot = lock(&self.rate_until);
        *slot = Some(slot.map_or(until, |current| current.max(until)));
    }

    pub(crate) async fn wait_rate_limit(&self) -> bool {
        loop {
            let until = *lock(&self.rate_until);
            let Some(until) = until else {
                return true;
            };
            if self.clock.unix_secs() >= until {
                return true;
            }
            if *self.stop.borrow() {
                return false;
            }
            let sleep = self.clock.sleep_until(until);
            tokio::pin!(sleep);
            let mut stop = self.stop.subscribe();
            tokio::select! {
                () = &mut sleep => {}
                _ = stop.changed() => {
                    if *stop.borrow() {
                        return false;
                    }
                }
            }
        }
    }

    pub(crate) async fn note_retry(&self) {
        let now = self.clock.unix_secs();
        lock(&self.retries).push(now);
        if let Err(error) = self.store.record_retry(now).await {
            tracing::warn!(%error, "retry was not stored");
        }
    }

    pub(crate) async fn record_core(
        &self,
        conversation: Option<ConversationId>,
        error: &hub_core::Error,
    ) -> Result<()> {
        let (retryable, reason) = match error {
            hub_core::Error::RateLimited { retry_after_secs } => (
                true,
                format!("rate limited, retry after {retry_after_secs}s"),
            ),
            hub_core::Error::TopicGone => (false, "topic is gone".to_owned()),
            hub_core::Error::Unsupported => (false, "unsupported".to_owned()),
            hub_core::Error::Adapter { detail } => (true, format!("adapter failed: {detail}")),
            other => (false, other.to_string()),
        };
        self.store
            .record_failure(crate::store::FailureRecord {
                conversation,
                reason: crate::store::clip_reason(&reason),
                retryable,
                created_at_unix_secs: self.clock.unix_secs(),
            })
            .await?;
        self.open_failures.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn cache_insert(cache: &mut Cache, mapping: &TopicMapping) {
    // A title learned in this process wins over the row we are restoring.
    cache
        .titles
        .entry(mapping.conversation.clone())
        .or_insert_with(|| mapping.title.clone());
    cache
        .applied
        .entry(mapping.conversation.clone())
        .or_insert_with(|| mapping.title.clone());
    cache
        .conversations
        .insert(mapping.topic, mapping.conversation.clone());
    cache
        .topics
        .insert(mapping.conversation.clone(), mapping.topic);
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lane_of_remote(event: &RemoteEvent) -> Option<String> {
    match event {
        RemoteEvent::Message(message) => Some(message.conversation.to_string()),
        RemoteEvent::Edit(edit) => Some(edit.target.conversation.to_string()),
        RemoteEvent::Unsend { target } | RemoteEvent::Reaction { target, .. } => {
            Some(target.conversation.to_string())
        }
        RemoteEvent::ChatInfo { conversation, .. }
        | RemoteEvent::GroupEvent { conversation, .. } => Some(conversation.to_string()),
        RemoteEvent::FriendRequest { .. } => Some(OPS_LANE.to_owned()),
        RemoteEvent::SessionChanged { .. } => None,
    }
}

fn topic_of(incoming: &TopicIncoming) -> TopicId {
    match incoming {
        TopicIncoming::Message { topic, .. }
        | TopicIncoming::Reaction { topic, .. }
        | TopicIncoming::Edit { topic, .. }
        | TopicIncoming::Unsend { topic, .. } => *topic,
    }
}
