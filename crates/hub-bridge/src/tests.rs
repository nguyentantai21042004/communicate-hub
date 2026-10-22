//! Invariant tests. Fakes only: no network, no Telegram, no sleeping on the wall clock
//! except a timeout that fails the test when a drain does not finish.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, ConversationKind, Mention,
    MessageRef, ModuleHealth, ModuleState, Network, NetworkAdapter, OutgoingMessage, RemoteEdit,
    RemoteEvent, RemoteMessage, SessionState, TopicSink,
};
use hub_store::{ContentKind, Direction};
use tokio::sync::{oneshot, watch};

use crate::limits::{
    AVATAR_CARD_FILE, RENAME_INTERVAL_SECS, SINK_MAX_ATTEMPTS, TOPIC_CONCURRENCY,
    TOPIC_RECREATE_LIMIT,
};
use crate::store::CountedDelivery;
use crate::{
    Bridge, BridgeConfig, BridgeStore, Error, ManualClock, MemoryStore, SqliteStore, TgMessageId,
    TopicId, TopicIncoming, TopicMapping,
};

const START_UNIX_SECS: i64 = 1_700_000_000;
const ORDER_TOPICS: usize = 20;
const ORDER_PER_TOPIC: usize = 50;
const LATENCY_SEED: u64 = 0x00C0_FFEE;
const DRAIN_BOUND: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockMode {
    Off,
    All,
    TitleHasBad,
}

enum Kind {
    Create,
    Send,
    Delete,
    React,
    Rename,
}

enum Wait {
    None,
    Yield,
    Block,
    BlockThenFail,
}

struct Data {
    next_topic: i32,
    next_message: i32,
    open: HashSet<i32>,
    titles: Vec<(TopicId, String)>,
    texts: Vec<(TopicId, String, Option<TgMessageId>)>,
    media: Vec<(TopicId, TgMessageId, Option<String>)>,
    media_files: Vec<Option<String>>,
    renames: Vec<(TopicId, String)>,
    reactions: Vec<(TopicId, TgMessageId, Option<String>)>,
    deleted: Vec<(TopicId, TgMessageId)>,
    attempts: u32,
    creates: u32,
    rate_limits_left: u32,
    retry_after_secs: u64,
    send_gone_left: u32,
    create_errors_left: u32,
    send_errors_left: u32,
    delete_gone: bool,
    rename_gone: bool,
    block: BlockMode,
    yield_latency: bool,
}

struct SinkState {
    data: Mutex<Data>,
    release: watch::Sender<bool>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    yield_seed: AtomicU64,
}

#[derive(Clone)]
struct FakeSink {
    state: Arc<SinkState>,
}

impl FakeSink {
    fn new() -> Self {
        let (release, _) = watch::channel(false);
        Self {
            state: Arc::new(SinkState {
                data: Mutex::new(Data {
                    next_topic: 0,
                    next_message: 100,
                    open: HashSet::new(),
                    titles: Vec::new(),
                    texts: Vec::new(),
                    media: Vec::new(),
                    media_files: Vec::new(),
                    renames: Vec::new(),
                    reactions: Vec::new(),
                    deleted: Vec::new(),
                    attempts: 0,
                    creates: 0,
                    rate_limits_left: 0,
                    retry_after_secs: 1,
                    send_gone_left: 0,
                    create_errors_left: 0,
                    send_errors_left: 0,
                    delete_gone: false,
                    rename_gone: false,
                    block: BlockMode::Off,
                    yield_latency: false,
                }),
                release,
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                yield_seed: AtomicU64::new(1),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Data> {
        self.state
            .data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_rate_limits(&self, left: u32, retry_after_secs: u64) {
        let mut data = self.lock();
        data.rate_limits_left = left;
        data.retry_after_secs = retry_after_secs;
    }

    fn set_send_gone(&self, left: u32) {
        self.lock().send_gone_left = left;
    }

    fn set_create_errors(&self, left: u32) {
        self.lock().create_errors_left = left;
    }

    fn set_send_errors(&self, left: u32) {
        self.lock().send_errors_left = left;
    }

    fn set_delete_gone(&self, enabled: bool) {
        self.lock().delete_gone = enabled;
    }

    fn set_rename_gone(&self, enabled: bool) {
        self.lock().rename_gone = enabled;
    }

    fn set_block(&self, mode: BlockMode) {
        self.lock().block = mode;
    }

    fn set_yield_latency(&self, seed: u64) {
        self.state.yield_seed.store(seed, Ordering::SeqCst);
        self.lock().yield_latency = true;
    }

    fn release(&self) {
        let _ = self.state.release.send(true);
    }

    fn open_topic(&self, topic: TopicId) {
        self.lock().open.insert(topic.0);
    }

    fn attempts(&self) -> u32 {
        self.lock().attempts
    }

    fn creates(&self) -> u32 {
        self.lock().creates
    }

    fn titles(&self) -> Vec<(TopicId, String)> {
        self.lock().titles.clone()
    }

    fn texts(&self) -> Vec<(TopicId, String, Option<TgMessageId>)> {
        self.lock().texts.clone()
    }

    fn media(&self) -> Vec<(TopicId, TgMessageId, Option<String>)> {
        self.lock().media.clone()
    }

    fn media_files(&self) -> Vec<Option<String>> {
        self.lock().media_files.clone()
    }

    fn renames(&self) -> Vec<(TopicId, String)> {
        self.lock().renames.clone()
    }

    fn reactions(&self) -> Vec<(TopicId, TgMessageId, Option<String>)> {
        self.lock().reactions.clone()
    }

    fn deleted(&self) -> Vec<(TopicId, TgMessageId)> {
        self.lock().deleted.clone()
    }

    fn in_flight(&self) -> usize {
        self.state.in_flight.load(Ordering::SeqCst)
    }

    fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }

    fn decide(&self, topic: Option<TopicId>, kind: &Kind) -> hub_core::Result<Wait> {
        let mut data = self.lock();
        data.attempts += 1;
        if data.rate_limits_left > 0 {
            data.rate_limits_left -= 1;
            return Err(hub_core::Error::RateLimited {
                retry_after_secs: data.retry_after_secs,
            });
        }
        match kind {
            Kind::Create => {
                if data.create_errors_left > 0 {
                    data.create_errors_left -= 1;
                    return Err(hub_core::Error::Adapter {
                        detail: "create failed".into(),
                    });
                }
                Ok(Wait::None)
            }
            Kind::Send => {
                if data.send_gone_left > 0 {
                    data.send_gone_left -= 1;
                    return Err(hub_core::Error::TopicGone);
                }
                if data.send_errors_left > 0 {
                    data.send_errors_left -= 1;
                    return Err(hub_core::Error::Adapter {
                        detail: "send failed".into(),
                    });
                }
                let Some(topic) = topic else {
                    return Err(hub_core::Error::Adapter {
                        detail: "send without a topic".into(),
                    });
                };
                if !data.open.contains(&topic.0) {
                    return Err(hub_core::Error::TopicGone);
                }
                let bad = data
                    .titles
                    .iter()
                    .any(|(id, title)| *id == topic && title.contains("bad"));
                if data.block == BlockMode::All {
                    return Ok(Wait::Block);
                }
                if data.block == BlockMode::TitleHasBad && bad {
                    return Ok(Wait::BlockThenFail);
                }
                if data.yield_latency {
                    return Ok(Wait::Yield);
                }
                Ok(Wait::None)
            }
            Kind::Delete => {
                let Some(topic) = topic else {
                    return Err(hub_core::Error::TopicGone);
                };
                if data.delete_gone || !data.open.contains(&topic.0) {
                    return Err(hub_core::Error::TopicGone);
                }
                Ok(Wait::None)
            }
            Kind::React => {
                let Some(topic) = topic else {
                    return Err(hub_core::Error::TopicGone);
                };
                if !data.open.contains(&topic.0) {
                    return Err(hub_core::Error::TopicGone);
                }
                Ok(Wait::None)
            }
            Kind::Rename => {
                if data.rename_gone {
                    data.rename_gone = false;
                    return Err(hub_core::Error::TopicGone);
                }
                let Some(topic) = topic else {
                    return Err(hub_core::Error::TopicGone);
                };
                if !data.open.contains(&topic.0) {
                    return Err(hub_core::Error::TopicGone);
                }
                Ok(Wait::None)
            }
        }
    }

    async fn wait(&self, wait: Wait) -> hub_core::Result<()> {
        match wait {
            Wait::None => Ok(()),
            Wait::Yield => {
                let spins = self.spins();
                for _ in 0..spins {
                    tokio::task::yield_now().await;
                }
                Ok(())
            }
            Wait::Block => {
                self.block().await;
                Ok(())
            }
            Wait::BlockThenFail => {
                self.block().await;
                Err(hub_core::Error::Adapter {
                    detail: "send failed".into(),
                })
            }
        }
    }

    async fn block(&self) {
        let now = self.state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.max_in_flight.fetch_max(now, Ordering::SeqCst);
        let mut rx = self.state.release.subscribe();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn spins(&self) -> usize {
        let mut state = self.state.yield_seed.load(Ordering::SeqCst);
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.state.yield_seed.store(state, Ordering::SeqCst);
        (state % 5) as usize
    }

    fn alloc_topic(&self, title: String) -> TopicId {
        let mut data = self.lock();
        data.next_topic += 1;
        let topic = TopicId(data.next_topic);
        data.open.insert(topic.0);
        data.titles.push((topic, title));
        data.creates += 1;
        topic
    }

    fn alloc_message(&self) -> TgMessageId {
        let mut data = self.lock();
        data.next_message += 1;
        TgMessageId(data.next_message)
    }
}

impl TopicSink for FakeSink {
    async fn create_topic(&self, title: String) -> hub_core::Result<TopicId> {
        let wait = self.decide(None, &Kind::Create)?;
        self.wait(wait).await?;
        Ok(self.alloc_topic(title))
    }

    async fn close_topic(&self, topic: TopicId) -> hub_core::Result<()> {
        if !self.lock().open.remove(&topic.0) {
            return Err(hub_core::Error::TopicGone);
        }
        Ok(())
    }

    async fn send_text(
        &self,
        topic: TopicId,
        text_html: String,
        reply_to: Option<TgMessageId>,
    ) -> hub_core::Result<TgMessageId> {
        let wait = self.decide(Some(topic), &Kind::Send)?;
        self.wait(wait).await?;
        let id = self.alloc_message();
        self.lock().texts.push((topic, text_html, reply_to));
        Ok(id)
    }

    async fn send_media(
        &self,
        topic: TopicId,
        attachment: Attachment,
        caption: Option<String>,
    ) -> hub_core::Result<TgMessageId> {
        let file_name = attachment.file_name.clone();
        let wait = self.decide(Some(topic), &Kind::Send)?;
        self.wait(wait).await?;
        let id = self.alloc_message();
        let mut data = self.lock();
        data.media.push((topic, id, caption));
        data.media_files.push(file_name);
        Ok(id)
    }

    async fn rename_topic(&self, topic: TopicId, title: String) -> hub_core::Result<()> {
        let wait = self.decide(Some(topic), &Kind::Rename)?;
        self.wait(wait).await?;
        self.lock().renames.push((topic, title));
        Ok(())
    }

    async fn set_reaction(
        &self,
        topic: TopicId,
        tg_message: TgMessageId,
        emoji: Option<String>,
    ) -> hub_core::Result<()> {
        let wait = self.decide(Some(topic), &Kind::React)?;
        self.wait(wait).await?;
        self.lock().reactions.push((topic, tg_message, emoji));
        Ok(())
    }

    async fn delete_message(
        &self,
        topic: TopicId,
        tg_message: TgMessageId,
    ) -> hub_core::Result<()> {
        let wait = self.decide(Some(topic), &Kind::Delete)?;
        self.wait(wait).await?;
        self.lock().deleted.push((topic, tg_message));
        Ok(())
    }
}

struct AdapterState {
    incoming: VecDeque<RemoteEvent>,
    sent: Vec<OutgoingMessage>,
    reactions: Vec<(MessageRef, String, bool)>,
    unsent: Vec<MessageRef>,
    next_id: u64,
    reads: u64,
    send_errors_left: u32,
    probe: Option<MemoryStore>,
    restores_at_subscribe: Option<u64>,
}

#[derive(Clone)]
struct FakeAdapter {
    network: Network,
    state: Arc<Mutex<AdapterState>>,
}

impl FakeAdapter {
    fn new(network: Network) -> Self {
        Self {
            network,
            state: Arc::new(Mutex::new(AdapterState {
                incoming: VecDeque::new(),
                sent: Vec::new(),
                reactions: Vec::new(),
                unsent: Vec::new(),
                next_id: 0,
                reads: 0,
                send_errors_left: 0,
                probe: None,
                restores_at_subscribe: None,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AdapterState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, event: RemoteEvent) {
        self.lock().incoming.push_back(event);
    }

    fn watch_restores(&self, store: MemoryStore) {
        self.lock().probe = Some(store);
    }

    fn restores_seen(&self) -> Option<u64> {
        self.lock().restores_at_subscribe
    }

    fn set_send_errors(&self, left: u32) {
        self.lock().send_errors_left = left;
    }

    fn sent(&self) -> Vec<OutgoingMessage> {
        self.lock().sent.clone()
    }

    fn reactions(&self) -> Vec<(MessageRef, String, bool)> {
        self.lock().reactions.clone()
    }

    fn unsent(&self) -> Vec<MessageRef> {
        self.lock().unsent.clone()
    }
}

impl NetworkAdapter for FakeAdapter {
    fn network(&self) -> Network {
        self.network
    }

    async fn next_event(&self) -> hub_core::Result<Option<RemoteEvent>> {
        let mut state = self.lock();
        if state.reads == 0 {
            state.restores_at_subscribe = state.probe.as_ref().map(MemoryStore::restore_calls);
        }
        state.reads += 1;
        Ok(state.incoming.pop_front())
    }

    async fn send(&self, message: OutgoingMessage) -> hub_core::Result<MessageRef> {
        let mut state = self.lock();
        if state.send_errors_left > 0 {
            state.send_errors_left -= 1;
            return Err(hub_core::Error::Adapter {
                detail: "send failed".into(),
            });
        }
        state.next_id += 1;
        let reference = MessageRef {
            conversation: message.conversation.clone(),
            message_id: format!("out-{}", state.next_id),
        };
        state.sent.push(message);
        Ok(reference)
    }

    async fn react(&self, target: MessageRef, emoji: String, added: bool) -> hub_core::Result<()> {
        self.lock().reactions.push((target, emoji, added));
        Ok(())
    }

    async fn unsend(&self, target: MessageRef) -> hub_core::Result<()> {
        self.lock().unsent.push(target);
        Ok(())
    }

    async fn health(&self) -> hub_core::Result<ModuleHealth> {
        Ok(ModuleHealth {
            module: "fake".into(),
            state: ModuleState::Ok,
            detail: String::new(),
            updated_at_unix_secs: START_UNIX_SECS,
        })
    }
}

struct Harness {
    bridge: Bridge<FakeSink, MemoryStore, ManualClock>,
    sink: FakeSink,
    store: MemoryStore,
    clock: ManualClock,
    adapter: FakeAdapter,
}

fn harness() -> Harness {
    harness_with(BridgeConfig::default(), Network::Zalo)
}

fn harness_with(config: BridgeConfig, network: Network) -> Harness {
    let sink = FakeSink::new();
    let store = MemoryStore::new();
    let clock = ManualClock::new(START_UNIX_SECS);
    let adapter = FakeAdapter::new(network);
    let bridge = Bridge::new(
        config,
        vec![Box::new(adapter.clone())],
        sink.clone(),
        store.clone(),
        clock.clone(),
    );
    Harness {
        bridge,
        sink,
        store,
        clock,
        adapter,
    }
}

fn zalo_dm(external: &str) -> ConversationId {
    ConversationId {
        network: Network::Zalo,
        kind: ConversationKind::Dm,
        external_id: external.into(),
    }
}

fn zalo_group(external: &str) -> ConversationId {
    ConversationId {
        network: Network::Zalo,
        kind: ConversationKind::Group,
        external_id: external.into(),
    }
}

fn text_message(conversation: ConversationId, id: &str, sender: &str, text: &str) -> RemoteEvent {
    RemoteEvent::Message(RemoteMessage {
        conversation,
        message_id: id.into(),
        sender_id: format!("id-{sender}"),
        sender_display_name: sender.into(),
        text: text.into(),
        attachments: Vec::new(),
        reply_to: None,
        sent_at_unix_secs: START_UNIX_SECS,
        mentions: Vec::new(),
    })
}

fn photo(url: &str) -> Attachment {
    Attachment {
        kind: AttachmentKind::Photo,
        mime: Some("image/jpeg".into()),
        size_bytes: Some(8),
        source: AttachmentSource::Url(url.into()),
        file_name: Some("a.jpg".into()),
    }
}

fn body_of(html: &str) -> &str {
    html.rsplit_once('\n').map_or(html, |(_, body)| body)
}

async fn spin_until(mut ready: impl FnMut() -> bool) {
    let mut met = false;
    for _ in 0..20_000 {
        if ready() {
            met = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(met, "timed out waiting for a condition");
}

async fn release_pause(clock: &ManualClock, secs: i64) {
    clock.advance(secs);
    tokio::task::yield_now().await;
    clock.advance(0);
    tokio::task::yield_now().await;
}

#[tokio::test(flavor = "current_thread")]
async fn one_conversation_keeps_its_topic_across_a_new_bridge() {
    let h = harness();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 1);
    let first_topic = h.sink.titles()[0].0;

    let bridge = Bridge::new(
        BridgeConfig::default(),
        vec![Box::new(h.adapter.clone())],
        h.sink.clone(),
        h.store.clone(),
        h.clock.clone(),
    );
    bridge
        .handle_remote(text_message(zalo_dm("a"), "m2", "An", "again"))
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 1);
    assert_eq!(h.sink.texts().len(), 2);
    assert!(
        h.sink
            .texts()
            .iter()
            .all(|(topic, _, _)| *topic == first_topic)
    );
    let stored = h.store.topic_for(&zalo_dm("a")).await.unwrap().unwrap();
    assert_eq!(stored.topic, first_topic);
    assert_eq!(stored.title, "An");
}

#[tokio::test(flavor = "current_thread")]
async fn a_gone_topic_is_recreated_once_and_the_send_is_retried() {
    assert_eq!(TOPIC_RECREATE_LIMIT, 1);
    let h = harness();
    h.sink.set_send_gone(1);
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 2);
    assert_eq!(h.sink.texts().len(), 1);
    assert_eq!(body_of(&h.sink.texts()[0].1), "hello");
    assert!(h.store.failures().await.unwrap().is_empty());
    let mapping = h.store.topic_for(&zalo_dm("a")).await.unwrap().unwrap();
    assert_eq!(mapping.topic, h.sink.titles()[1].0);
    assert_eq!(mapping.title, "An");
    assert_eq!(h.sink.titles()[1].1, "🔵 👤 An");
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_topic_gone_stops_and_a_failed_create_stores_nothing() {
    let h = harness();
    h.sink.set_send_gone(10);
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 2);
    assert!(h.sink.texts().is_empty());
    assert!(h.store.topic_for(&zalo_dm("a")).await.unwrap().is_none());
    let failures = h.store.failures().await.unwrap();
    assert_eq!(failures.len(), 1);
    assert!(!failures[0].retryable);
    assert!(failures[0].reason.contains("topic is gone"));

    let h = harness();
    h.sink.set_create_errors(1);
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 0);
    assert!(h.store.topic_for(&zalo_dm("a")).await.unwrap().is_none());
    let failures = h.store.failures().await.unwrap();
    assert_eq!(failures.len(), 1);
    assert!(failures[0].retryable);
    assert!(failures[0].reason.contains("create failed"));
}

#[tokio::test(flavor = "current_thread")]
async fn unsend_of_a_deleted_topic_is_success() {
    let h = harness();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    h.sink.set_delete_gone(true);
    h.bridge
        .handle_remote(RemoteEvent::Unsend {
            target: MessageRef {
                conversation: zalo_dm("a"),
                message_id: "m1".into(),
            },
        })
        .await
        .unwrap();
    assert!(h.sink.deleted().is_empty());
    assert!(h.store.failures().await.unwrap().is_empty());
    assert!(h.store.topic_for(&zalo_dm("a")).await.unwrap().is_none());
    assert!(
        h.store
            .message_for_network(&zalo_dm("a"), "m1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn per_topic_order_holds_for_1000_events_across_20_topics() {
    let h = harness();
    h.sink.set_yield_latency(LATENCY_SEED);
    let mut tasks = Vec::with_capacity(ORDER_TOPICS * ORDER_PER_TOPIC);
    for topic in 0..ORDER_TOPICS {
        for seq in 0..ORDER_PER_TOPIC {
            let bridge = h.bridge.clone();
            let conversation = zalo_dm(&format!("c{topic}"));
            tasks.push(tokio::spawn(async move {
                bridge
                    .handle_remote(text_message(
                        conversation,
                        &format!("t{topic}-{seq}"),
                        "An",
                        &format!("{seq}"),
                    ))
                    .await
            }));
        }
    }
    let finished = tokio::time::timeout(Duration::from_secs(5), async {
        for task in tasks {
            task.await.unwrap().unwrap();
        }
    })
    .await;
    assert!(
        finished.is_ok(),
        "ordered delivery did not finish within 5s"
    );
    assert_eq!(h.sink.creates() as usize, ORDER_TOPICS);
    let mut by_topic: HashMap<TopicId, Vec<usize>> = HashMap::new();
    for (topic, html, _) in h.sink.texts() {
        let seq = body_of(&html).parse::<usize>().unwrap();
        by_topic.entry(topic).or_default().push(seq);
    }
    assert_eq!(by_topic.len(), ORDER_TOPICS);
    for seqs in by_topic.values() {
        let expected: Vec<usize> = (0..ORDER_PER_TOPIC).collect();
        assert_eq!(seqs, &expected);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn topics_run_up_to_the_concurrency_cap() {
    assert_eq!(TOPIC_CONCURRENCY, 8);
    let h = harness();
    h.sink.set_block(BlockMode::All);
    let mut tasks = Vec::new();
    for index in 0..ORDER_TOPICS {
        let bridge = h.bridge.clone();
        tasks.push(tokio::spawn(async move {
            bridge
                .handle_remote(text_message(
                    zalo_dm(&format!("c{index}")),
                    "m1",
                    "An",
                    "hi",
                ))
                .await
        }));
    }
    spin_until(|| h.sink.in_flight() >= TOPIC_CONCURRENCY).await;
    assert_eq!(h.sink.max_in_flight(), TOPIC_CONCURRENCY);
    assert!(h.sink.in_flight() <= TOPIC_CONCURRENCY);
    h.sink.release();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(h.sink.creates() as usize, ORDER_TOPICS);
    assert_eq!(h.sink.texts().len(), ORDER_TOPICS);
}

#[tokio::test(flavor = "current_thread")]
async fn duplicates_are_dropped_and_the_same_id_in_two_chats_both_send() {
    let h = harness();
    h.sink.set_block(BlockMode::All);
    let first = h.bridge.clone();
    let task = tokio::spawn(async move {
        first
            .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
            .await
    });
    spin_until(|| h.sink.in_flight() >= 1).await;
    let second = h.bridge.clone();
    let duplicate = tokio::spawn(async move {
        second
            .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
            .await
    });
    h.sink.release();
    task.await.unwrap().unwrap();
    duplicate.await.unwrap().unwrap();
    assert_eq!(h.sink.texts().len(), 1);

    h.bridge
        .handle_remote(text_message(zalo_dm("b"), "m1", "Binh", "other"))
        .await
        .unwrap();
    assert_eq!(h.sink.texts().len(), 2);
    assert_eq!(h.sink.creates(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn a_media_group_maps_only_its_first_message() {
    let h = harness();
    h.bridge
        .handle_remote(RemoteEvent::Message(RemoteMessage {
            conversation: zalo_dm("a"),
            message_id: "album-1".into(),
            sender_id: "id-An".into(),
            sender_display_name: "An".into(),
            text: "album".into(),
            attachments: vec![
                photo("https://cdn.example/1.jpg"),
                photo("https://cdn.example/2.jpg"),
                photo("https://cdn.example/3.jpg"),
            ],
            reply_to: None,
            sent_at_unix_secs: START_UNIX_SECS,
            mentions: Vec::new(),
        }))
        .await
        .unwrap();
    let media = h.sink.media();
    assert_eq!(media.len(), 3);
    assert_eq!(media[0].2.as_deref(), Some("An: album"));
    assert_eq!(media[1].2, None);
    assert_eq!(media[2].2, None);
    assert!(h.sink.texts().is_empty());
    let mapped = h
        .store
        .message_for_network(&zalo_dm("a"), "album-1")
        .await
        .unwrap();
    assert_eq!(mapped, Some(media[0].1));
    assert!(
        h.store
            .message_for_telegram(media[1].1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        h.store
            .message_for_telegram(media[2].1)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reply_reaction_unsend_and_edit_map_both_ways() {
    let h = harness();
    let conversation = zalo_dm("a");
    h.bridge
        .handle_remote(text_message(conversation.clone(), "m1", "An", "hello"))
        .await
        .unwrap();
    let topic = h.sink.titles()[0].0;
    let first_tg = h
        .store
        .message_for_network(&conversation, "m1")
        .await
        .unwrap()
        .unwrap();

    let mut reply = text_message(conversation.clone(), "m2", "An", "reply");
    if let RemoteEvent::Message(body) = &mut reply {
        body.reply_to = Some(MessageRef {
            conversation: conversation.clone(),
            message_id: "m1".into(),
        });
    }
    h.bridge.handle_remote(reply).await.unwrap();
    assert_eq!(h.sink.texts()[1].2, Some(first_tg));

    h.bridge
        .handle_remote(RemoteEvent::Reaction {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "m1".into(),
            },
            emoji: "/-heart".into(),
            added: true,
        })
        .await
        .unwrap();
    assert_eq!(h.sink.reactions()[0].2.as_deref(), Some("❤️"));

    h.bridge
        .handle_remote(RemoteEvent::Edit(RemoteEdit {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "m1".into(),
            },
            text: "changed".into(),
            attachments: Vec::new(),
            edited_at_unix_secs: START_UNIX_SECS,
        }))
        .await
        .unwrap();
    assert_eq!(h.sink.texts()[2].2, Some(first_tg));
    assert_eq!(body_of(&h.sink.texts()[2].1), "changed");
    assert_eq!(
        h.store
            .message_for_network(&conversation, "m1")
            .await
            .unwrap(),
        Some(first_tg)
    );

    let human = TgMessageId(50);
    h.bridge
        .handle_topic_message(TopicIncoming::Message {
            topic,
            tg_message: human,
            text: "pong".into(),
            attachments: Vec::new(),
            reply_to: Some(first_tg),
        })
        .await
        .unwrap();
    let sent = h.adapter.sent();
    assert_eq!(sent[0].text, "pong");
    assert_eq!(
        sent[0]
            .reply_to
            .as_ref()
            .map(|item| item.message_id.as_str()),
        Some("m1")
    );
    let network_id = h.store.message_for_telegram(human).await.unwrap().unwrap();
    assert_eq!(network_id.message_id, "out-1");

    h.bridge
        .handle_remote(text_message(conversation.clone(), "out-1", "An", "pong"))
        .await
        .unwrap();
    assert_eq!(h.sink.texts().len(), 3);

    h.bridge
        .handle_topic_message(TopicIncoming::Reaction {
            topic,
            tg_message: first_tg,
            emoji: "👍".into(),
            added: true,
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.reactions()[0].1, "/-strong");
    assert!(h.adapter.reactions()[0].2);

    h.bridge
        .handle_topic_message(TopicIncoming::Edit {
            topic,
            tg_message: first_tg,
            text: "edited on telegram".into(),
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.sent()[1].text, "edited on telegram");
    assert_eq!(
        h.adapter.sent()[1]
            .reply_to
            .as_ref()
            .map(|item| item.message_id.as_str()),
        Some("m1")
    );

    h.bridge
        .handle_topic_message(TopicIncoming::Unsend {
            topic,
            tg_message: human,
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.unsent()[0].message_id, "out-1");
    assert!(h.store.message_for_telegram(human).await.unwrap().is_none());

    h.bridge
        .handle_remote(RemoteEvent::Unsend {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "m1".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(h.sink.deleted()[0].1, first_tg);
    assert!(h.store.failures().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_targets_become_plain_messages() {
    let h = harness();
    let conversation = zalo_dm("a");
    h.bridge
        .handle_remote(text_message(conversation.clone(), "m1", "An", "hello"))
        .await
        .unwrap();
    let topic = h.sink.titles()[0].0;
    let before = h.store.failures().await.unwrap().len();

    let mut reply = text_message(conversation.clone(), "m2", "An", "orphan reply");
    if let RemoteEvent::Message(body) = &mut reply {
        body.reply_to = Some(MessageRef {
            conversation: conversation.clone(),
            message_id: "missing".into(),
        });
    }
    h.bridge.handle_remote(reply).await.unwrap();
    assert_eq!(h.sink.texts()[1].2, None);

    h.bridge
        .handle_remote(RemoteEvent::Reaction {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "missing".into(),
            },
            emoji: "👍".into(),
            added: true,
        })
        .await
        .unwrap();
    assert!(body_of(&h.sink.texts()[2].1).contains("reaction"));

    h.bridge
        .handle_remote(RemoteEvent::Unsend {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "missing".into(),
            },
        })
        .await
        .unwrap();
    assert!(h.sink.texts()[3].1.contains("message removed"));

    h.bridge
        .handle_remote(RemoteEvent::Edit(RemoteEdit {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "missing".into(),
            },
            text: "plain edit".into(),
            attachments: Vec::new(),
            edited_at_unix_secs: START_UNIX_SECS,
        }))
        .await
        .unwrap();
    assert_eq!(h.sink.texts()[4].2, None);
    assert_eq!(body_of(&h.sink.texts()[4].1), "plain edit");

    let reactions_before = h.sink.reactions().len();
    h.bridge
        .handle_remote(RemoteEvent::Reaction {
            target: MessageRef {
                conversation: conversation.clone(),
                message_id: "m1".into(),
            },
            emoji: "🌹".into(),
            added: true,
        })
        .await
        .unwrap();
    assert_eq!(h.sink.reactions().len(), reactions_before);

    let mapped = h
        .store
        .message_for_network(&conversation, "m1")
        .await
        .unwrap()
        .unwrap();
    let sent_before = h.adapter.sent().len();
    h.bridge
        .handle_topic_message(TopicIncoming::Reaction {
            topic,
            tg_message: mapped,
            emoji: "🦄".into(),
            added: true,
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.sent().len(), sent_before);
    assert!(h.adapter.reactions().is_empty());

    h.bridge
        .handle_topic_message(TopicIncoming::Reaction {
            topic,
            tg_message: TgMessageId(999),
            emoji: "👍".into(),
            added: true,
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.sent().last().unwrap().text, "👍");

    h.bridge
        .handle_topic_message(TopicIncoming::Message {
            topic: TopicId(404),
            tg_message: TgMessageId(1),
            text: "lost".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap();
    let failures = h.store.failures().await.unwrap();
    assert!(failures.len() > before);
    assert!(
        failures
            .iter()
            .any(|failure| !failure.retryable && failure.reason.contains("unmapped topic"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn rate_limit_pauses_every_topic_then_resumes() {
    let h = harness();
    h.sink.set_rate_limits(1, 5);
    let bridge = h.bridge.clone();
    let task = tokio::spawn(async move {
        bridge
            .handle_remote(text_message(zalo_dm("a"), "m1", "An", "one"))
            .await
    });
    spin_until(|| h.bridge.health().detail.contains("rate_limited=true")).await;
    assert_eq!(h.sink.attempts(), 1);
    assert!(h.bridge.health().detail.contains("retries_24h=1"));
    assert_eq!(h.bridge.health().state, ModuleState::Degraded);

    let bridge = h.bridge.clone();
    let second = tokio::spawn(async move {
        bridge
            .handle_remote(text_message(zalo_dm("b"), "m1", "Binh", "two"))
            .await
    });
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(h.sink.attempts(), 1);
    release_pause(&h.clock, 5).await;
    task.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(h.sink.attempts(), 5);
    assert_eq!(h.sink.texts().len(), 2);
    assert!(h.store.failures().await.unwrap().is_empty());
    assert!(h.bridge.health().detail.contains("rate_limited=false"));
}

#[tokio::test(flavor = "current_thread")]
async fn rate_limit_gives_up_after_the_attempt_cap() {
    assert_eq!(SINK_MAX_ATTEMPTS, 3);
    let h = harness();
    h.sink.set_rate_limits(100, 3);
    let bridge = h.bridge.clone();
    let task = tokio::spawn(async move {
        bridge
            .handle_remote(text_message(zalo_dm("a"), "m1", "An", "one"))
            .await
    });
    for _ in 0..1_000 {
        if task.is_finished() {
            break;
        }
        if h.bridge.health().detail.contains("rate_limited=true") {
            release_pause(&h.clock, 3).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    assert!(task.is_finished());
    task.await.unwrap().unwrap();
    assert_eq!(h.sink.attempts(), SINK_MAX_ATTEMPTS);
    assert!(h.sink.texts().is_empty());
    let failures = h.store.failures().await.unwrap();
    assert_eq!(failures.len(), 1);
    assert!(failures[0].retryable);
    assert!(failures[0].reason.contains("rate limited"));
    assert!(h.bridge.health().detail.contains("retries_24h=2"));
    assert_eq!(h.bridge.health().state, ModuleState::Degraded);
}

#[tokio::test(flavor = "current_thread")]
async fn one_failing_topic_does_not_block_another() {
    let h = harness();
    h.sink.set_block(BlockMode::TitleHasBad);
    let bridge = h.bridge.clone();
    let bad = tokio::spawn(async move {
        bridge
            .handle_remote(text_message(zalo_dm("a"), "m1", "bad", "nope"))
            .await
    });
    spin_until(|| h.sink.in_flight() >= 1).await;
    tokio::time::timeout(
        Duration::from_secs(1),
        h.bridge
            .handle_remote(text_message(zalo_dm("b"), "m1", "good", "yes")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(h.sink.texts().len(), 1);
    assert_eq!(body_of(&h.sink.texts()[0].1), "yes");
    h.sink.release();
    bad.await.unwrap().unwrap();
    assert!(
        h.store
            .failures()
            .await
            .unwrap()
            .iter()
            .any(|failure| failure.reason.contains("send failed"))
    );

    let h = harness();
    h.sink.set_send_errors(1);
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "first"))
        .await
        .unwrap();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m2", "An", "second"))
        .await
        .unwrap();
    assert_eq!(h.sink.texts().len(), 1);
    assert_eq!(body_of(&h.sink.texts()[0].1), "second");
    assert_eq!(h.store.failures().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_drains_the_queue_within_a_bound() {
    let h = harness();
    h.sink.set_block(BlockMode::All);
    let (tx, rx) = oneshot::channel();
    let bridge = h.bridge.clone();
    let run = tokio::spawn(async move {
        bridge
            .run(async {
                let _ = rx.await;
            })
            .await
    });
    spin_until(|| h.bridge.health().state == ModuleState::Ok).await;
    let mut tasks = Vec::new();
    for index in 0..10 {
        let bridge = h.bridge.clone();
        tasks.push(tokio::spawn(async move {
            bridge
                .handle_remote(text_message(
                    zalo_dm("a"),
                    &format!("m{index}"),
                    "An",
                    &format!("{index}"),
                ))
                .await
        }));
    }
    spin_until(|| h.bridge.health().detail.starts_with("queue_depth=10 ")).await;
    let _ = tx.send(());
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        !run.is_finished(),
        "shutdown returned before the queue drained"
    );
    h.sink.release();
    tokio::time::timeout(DRAIN_BOUND, run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    let bodies: Vec<usize> = h
        .sink
        .texts()
        .iter()
        .map(|(_, html, _)| body_of(html).parse().unwrap())
        .collect();
    assert_eq!(bodies, (0..10).collect::<Vec<_>>());
    let err = h
        .bridge
        .handle_remote(text_message(zalo_dm("a"), "late", "An", "nope"))
        .await
        .unwrap_err();
    assert_eq!(err, Error::ShutDown);
    assert_eq!(h.bridge.health().state, ModuleState::Down);
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_cancels_a_rate_limit_wait() {
    let h = harness();
    h.sink.set_rate_limits(100, 10_000);
    let (tx, rx) = oneshot::channel();
    let bridge = h.bridge.clone();
    let run = tokio::spawn(async move {
        bridge
            .run(async {
                let _ = rx.await;
            })
            .await
    });
    spin_until(|| h.bridge.health().state == ModuleState::Ok).await;
    let bridge = h.bridge.clone();
    let task = tokio::spawn(async move {
        bridge
            .handle_remote(text_message(zalo_dm("a"), "m1", "An", "one"))
            .await
    });
    spin_until(|| h.bridge.health().detail.contains("rate_limited=true")).await;
    let _ = tx.send(());
    tokio::time::timeout(DRAIN_BOUND, run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    task.await.unwrap().unwrap();
    let failures = h.store.failures().await.unwrap();
    assert!(
        failures
            .iter()
            .any(|failure| failure.retryable && failure.reason.contains("shut down"))
    );
    assert!(h.sink.texts().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn startup_restores_mappings_before_subscribe() {
    let h = harness();
    h.store
        .upsert_topic(crate::TopicMapping {
            conversation: zalo_dm("a"),
            topic: TopicId(4),
            title: "An".into(),
        })
        .await
        .unwrap();
    h.sink.open_topic(TopicId(4));
    h.adapter
        .push(text_message(zalo_dm("a"), "m1", "An", "restored"));
    h.adapter.watch_restores(h.store.clone());
    let (tx, rx) = oneshot::channel();
    let bridge = h.bridge.clone();
    let run = tokio::spawn(async move {
        bridge
            .run(async {
                let _ = rx.await;
            })
            .await
    });
    spin_until(|| h.sink.texts().len() == 1).await;
    assert!(h.adapter.restores_seen().unwrap() >= 1);
    assert_eq!(h.sink.creates(), 0);
    assert_eq!(body_of(&h.sink.texts()[0].1), "restored");
    let _ = tx.send(());
    run.await.unwrap().unwrap();

    let again = h.bridge.run(async {}).await.unwrap_err();
    assert_eq!(again, Error::ShutDown);
}

#[tokio::test(flavor = "current_thread")]
async fn health_reports_queue_retries_and_rate_limit() {
    let h = harness();
    let health = h.bridge.health();
    assert_eq!(health.module, "bridge");
    assert_eq!(health.state, ModuleState::Unknown);
    assert_eq!(
        health.detail,
        "queue_depth=0 retries_24h=0 rate_limited=false"
    );
    assert_eq!(health.updated_at_unix_secs, START_UNIX_SECS);

    h.store.record_retry(START_UNIX_SECS).await.unwrap();
    h.bridge.run(async {}).await.unwrap();
    let health = h.bridge.health();
    assert_eq!(health.state, ModuleState::Down);
    assert!(health.detail.contains("retries_24h=1"));
    assert!(health.detail.contains("queue_depth=0"));
    assert!(health.detail.contains("rate_limited=false"));
}

#[tokio::test(flavor = "current_thread")]
async fn mentions_resolve_both_ways() {
    let h = harness();
    let conversation = zalo_dm("a");
    let mut message = text_message(conversation.clone(), "m1", "An", "hi Nguyễn Tấn Tài");
    if let RemoteEvent::Message(body) = &mut message {
        body.mentions = vec![
            Mention {
                user_id: "u-tai".into(),
                display_name: "Nguyễn Tấn Tài".into(),
            },
            Mention {
                user_id: "u-other".into(),
                display_name: "Tài".into(),
            },
        ];
        body.sender_id = "u-an".into();
        body.sender_display_name = "An".into();
    }
    h.bridge.handle_remote(message).await.unwrap();
    assert!(h.sink.texts()[0].1.contains("<b>Nguyễn Tấn Tài</b>"));

    let topic = h.sink.titles()[0].0;
    h.bridge
        .handle_topic_message(TopicIncoming::Message {
            topic,
            tg_message: TgMessageId(80),
            text: "cc @Nguyễn Tấn Tài xem".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap();
    assert_eq!(h.adapter.sent()[0].text, "cc @Nguyễn Tấn Tài xem");
    let log = h.store.lookup_log();
    assert!(
        log.iter()
            .any(|(name, id)| name == "nguyen tan tai" && id.as_deref() == Some("u-tai"))
    );
    assert!(
        log.iter()
            .any(|(name, id)| name == "nguyen" && id.as_deref() != Some("u-tai"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn formatting_escapes_truncates_and_prefixes_the_topic() {
    let h = harness();
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_group("g1"),
            title: Some("Team & Co".into()),
            avatar: None,
        })
        .await
        .unwrap();
    h.bridge
        .handle_remote(text_message(
            zalo_group("g1"),
            "m1",
            "A & B",
            "<b>a & b</b>",
        ))
        .await
        .unwrap();
    assert_eq!(h.sink.titles()[0].1, "🔵 👥 Team & Co");
    assert_eq!(
        h.sink.texts()[0].1,
        "<b>A &amp; B:</b>\n&lt;b&gt;a &amp; b&lt;/b&gt;"
    );

    let long = "x".repeat(hub_core::limits::TEXT_MAX_UTF16 + 80);
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "Nhóm   giao  hàng", &long))
        .await
        .unwrap();
    assert_eq!(h.sink.titles()[1].1, "🔵 👤 Nhóm giao hàng");
    let html = &h.sink.texts()[1].1;
    assert!(html.ends_with('\u{2026}'));
    assert!(crate::format::utf16_len(html) <= hub_core::limits::TEXT_MAX_UTF16);

    h.bridge
        .handle_remote(RemoteEvent::GroupEvent {
            conversation: zalo_group("g1"),
            text: "An joined".into(),
            sent_at_unix_secs: START_UNIX_SECS,
        })
        .await
        .unwrap();
    assert!(
        h.sink
            .texts()
            .iter()
            .any(|(_, html, _)| html == "<i>An joined</i>")
    );

    h.sink.set_send_gone(1);
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_dm("a"),
            title: Some("An Nguyen".into()),
            avatar: None,
        })
        .await
        .unwrap();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m2", "An", "next"))
        .await
        .unwrap();
    assert!(
        h.sink
            .titles()
            .iter()
            .any(|(_, title)| title == "🔵 👤 An Nguyen")
    );
    assert!(
        !h.sink
            .titles()
            .iter()
            .any(|(_, title)| title.contains("🔵 🔵"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ops_topic_receives_session_and_friend_events() {
    let h = harness_with(
        BridgeConfig {
            ops_topic: Some(TopicId(7)),
            ..BridgeConfig::default()
        },
        Network::Zalo,
    );
    h.sink.open_topic(TopicId(7));
    h.bridge
        .handle_remote(RemoteEvent::SessionChanged {
            state: SessionState::Expired,
        })
        .await
        .unwrap();
    h.bridge
        .handle_remote(RemoteEvent::FriendRequest {
            sender_id: "u-9".into(),
            sender_display_name: "Chi".into(),
            message: String::new(),
        })
        .await
        .unwrap();
    let texts = h.sink.texts();
    assert!(texts[0].1.contains("session expired"));
    assert!(texts[1].1.contains("friend request from Chi"));
    assert!(h.store.failures().await.unwrap().is_empty());

    let quiet = harness();
    quiet
        .bridge
        .handle_remote(RemoteEvent::SessionChanged {
            state: SessionState::Valid,
        })
        .await
        .unwrap();
    assert!(quiet.sink.texts().is_empty());
    assert!(quiet.store.failures().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_run_while_running_is_rejected() {
    let h = harness();
    let (tx, rx) = oneshot::channel();
    let bridge = h.bridge.clone();
    let run = tokio::spawn(async move {
        bridge
            .run(async {
                let _ = rx.await;
            })
            .await
    });
    spin_until(|| h.bridge.health().state == ModuleState::Ok).await;
    let err = h.bridge.run(async {}).await.unwrap_err();
    assert_eq!(err, Error::AlreadyRunning);
    let _ = tx.send(());
    run.await.unwrap().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_known_title_renames_once_inside_the_window() {
    let h = harness();
    h.bridge
        .handle_remote(text_message(zalo_group("g1"), "m1", "", "hello"))
        .await
        .unwrap();
    assert_eq!(h.sink.titles()[0].1, "🔵 👥 g1");
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_group("g1"),
            title: Some("Team".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert_eq!(
        h.sink.renames(),
        vec![(h.sink.titles()[0].0, "🔵 👥 Team".into())]
    );
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_group("g1"),
            title: Some("Team Two".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert_eq!(h.sink.renames().len(), 1);
    h.clock.advance(RENAME_INTERVAL_SECS);
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_group("g1"),
            title: Some("Team Two".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert_eq!(h.sink.renames().len(), 2);
    assert_eq!(
        h.store.title(&zalo_group("g1")).await.unwrap().as_deref(),
        Some("Team Two")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_missing_topic_on_rename_is_recreated_once() {
    let h = harness();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    h.sink.set_rename_gone(true);
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_dm("a"),
            title: Some("An Nguyen".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert_eq!(h.sink.creates(), 2);
    assert_eq!(h.sink.renames().len(), 0);
    assert_eq!(h.sink.titles()[1].1, "🔵 👤 An Nguyen");
    assert!(h.store.failures().await.unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn an_avatar_card_is_posted_once() {
    let h = harness();
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_dm("a"),
            title: Some("An".into()),
            avatar: Some(AttachmentSource::Url("https://cdn.example/an.jpg".into())),
        })
        .await
        .unwrap();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    h.bridge
        .handle_remote(text_message(zalo_dm("a"), "m2", "An", "again"))
        .await
        .unwrap();
    let cards = h
        .sink
        .media_files()
        .into_iter()
        .filter(|name| name.as_deref() == Some(AVATAR_CARD_FILE))
        .count();
    assert_eq!(cards, 1);
    assert!(h.store.avatar_posted(&zalo_dm("a")).await.unwrap());

    let quiet = harness_with(
        BridgeConfig {
            avatar_card: false,
            ..BridgeConfig::default()
        },
        Network::Zalo,
    );
    quiet
        .bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: zalo_dm("a"),
            title: Some("An".into()),
            avatar: Some(AttachmentSource::Url("https://cdn.example/an.jpg".into())),
        })
        .await
        .unwrap();
    quiet
        .bridge
        .handle_remote(text_message(zalo_dm("a"), "m1", "An", "hello"))
        .await
        .unwrap();
    assert!(quiet.sink.media_files().iter().all(|name| name.is_none()));
}

#[tokio::test(flavor = "current_thread")]
async fn an_imported_title_is_not_renamed() {
    let h = harness();
    let conversation = zalo_group("g-old");
    let topic = TopicId(40);
    h.sink.open_topic(topic);
    h.store
        .upsert_topic(TopicMapping {
            conversation: conversation.clone(),
            topic,
            title: "Gia Dinh".into(),
        })
        .await
        .unwrap();
    h.bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: conversation.clone(),
            title: Some("Gia Dinh".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert!(h.sink.renames().is_empty());
    assert_eq!(h.sink.creates(), 0);
}

struct BareSink;

impl TopicSink for BareSink {
    async fn create_topic(&self, _title: String) -> hub_core::Result<TopicId> {
        Ok(TopicId(1))
    }

    async fn close_topic(&self, _topic: TopicId) -> hub_core::Result<()> {
        Ok(())
    }

    async fn send_text(
        &self,
        _topic: TopicId,
        _text_html: String,
        _reply_to: Option<TgMessageId>,
    ) -> hub_core::Result<TgMessageId> {
        Ok(TgMessageId(1))
    }

    async fn send_media(
        &self,
        _topic: TopicId,
        _attachment: Attachment,
        _caption: Option<String>,
    ) -> hub_core::Result<TgMessageId> {
        Ok(TgMessageId(2))
    }

    async fn set_reaction(
        &self,
        _topic: TopicId,
        _tg_message: TgMessageId,
        _emoji: Option<String>,
    ) -> hub_core::Result<()> {
        Ok(())
    }

    async fn delete_message(
        &self,
        _topic: TopicId,
        _tg_message: TgMessageId,
    ) -> hub_core::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unsupported_rename_is_not_a_failure() {
    let store = MemoryStore::new();
    let clock = ManualClock::new(START_UNIX_SECS);
    let adapter = FakeAdapter::new(Network::Zalo);
    let bridge = Bridge::new(
        BridgeConfig::default(),
        vec![Box::new(adapter)],
        BareSink,
        store.clone(),
        clock,
    );
    let conversation = zalo_dm("a");
    store
        .upsert_topic(TopicMapping {
            conversation: conversation.clone(),
            topic: TopicId(3),
            title: "id".into(),
        })
        .await
        .unwrap();
    bridge
        .handle_remote(RemoteEvent::ChatInfo {
            conversation: conversation.clone(),
            title: Some("An".into()),
            avatar: None,
        })
        .await
        .unwrap();
    assert!(store.failures().await.unwrap().is_empty());
    assert_eq!(
        store.title(&conversation).await.unwrap().as_deref(),
        Some("An")
    );
}

const BODY: &str = "counter-body-secret";
const OUT_BODY: &str = "counter-out-secret";
const EDIT_BODY: &str = "counter-edit-secret";
const SENT_AT: i64 = START_UNIX_SECS + 3_600;
const OUT_AT: i64 = START_UNIX_SECS + 7_200;

fn counted_message(id: &str, text: &str, attachments: Vec<Attachment>) -> RemoteEvent {
    RemoteEvent::Message(RemoteMessage {
        conversation: zalo_dm("a"),
        message_id: id.into(),
        sender_id: "id-An".into(),
        sender_display_name: "An".into(),
        text: text.into(),
        attachments,
        reply_to: None,
        sent_at_unix_secs: SENT_AT,
        mentions: Vec::new(),
    })
}

fn file_attachment(kind: AttachmentKind, name: &str) -> Attachment {
    Attachment {
        kind,
        mime: None,
        size_bytes: Some(4),
        source: AttachmentSource::Url(format!("https://cdn.example/{name}")),
        file_name: Some(name.into()),
    }
}

async fn exercise_counts<St>(
    bridge: &Bridge<FakeSink, St, ManualClock>,
    sink: &FakeSink,
    adapter: &FakeAdapter,
    clock: &ManualClock,
) where
    St: BridgeStore + 'static,
{
    bridge
        .handle_remote(counted_message("m-text", BODY, Vec::new()))
        .await
        .unwrap();
    bridge
        .handle_remote(counted_message("m-text", BODY, Vec::new()))
        .await
        .unwrap();
    bridge
        .handle_remote(counted_message(
            "m-photo",
            "caption",
            vec![photo("https://cdn.example/one.jpg")],
        ))
        .await
        .unwrap();
    bridge
        .handle_remote(counted_message(
            "m-album",
            "album",
            vec![
                photo("https://cdn.example/1.jpg"),
                photo("https://cdn.example/2.jpg"),
                photo("https://cdn.example/3.jpg"),
            ],
        ))
        .await
        .unwrap();
    bridge
        .handle_remote(counted_message(
            "m-voice",
            "",
            vec![file_attachment(AttachmentKind::Voice, "note.ogg")],
        ))
        .await
        .unwrap();
    bridge
        .handle_remote(counted_message(
            "m-sticker",
            "",
            vec![file_attachment(AttachmentKind::Sticker, "smile.webp")],
        ))
        .await
        .unwrap();
    bridge
        .handle_remote(RemoteEvent::Edit(RemoteEdit {
            target: MessageRef {
                conversation: zalo_dm("a"),
                message_id: "m-text".into(),
            },
            text: EDIT_BODY.into(),
            attachments: Vec::new(),
            edited_at_unix_secs: SENT_AT,
        }))
        .await
        .unwrap();
    bridge
        .handle_remote(RemoteEvent::Reaction {
            target: MessageRef {
                conversation: zalo_dm("a"),
                message_id: "m-text".into(),
            },
            emoji: "/-heart".into(),
            added: true,
        })
        .await
        .unwrap();
    bridge
        .handle_remote(RemoteEvent::GroupEvent {
            conversation: zalo_dm("a"),
            text: "An joined".into(),
            sent_at_unix_secs: SENT_AT,
        })
        .await
        .unwrap();
    bridge
        .handle_remote(RemoteEvent::Unsend {
            target: MessageRef {
                conversation: zalo_dm("a"),
                message_id: "m-photo".into(),
            },
        })
        .await
        .unwrap();
    sink.set_send_errors(1);
    bridge
        .handle_remote(counted_message("m-fail", "nope", Vec::new()))
        .await
        .unwrap();
    let topic = sink.titles()[0].0;
    clock.advance(7_200);
    bridge
        .handle_topic_message(TopicIncoming::Message {
            topic,
            tg_message: TgMessageId(50),
            text: OUT_BODY.into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap();
    bridge
        .handle_topic_message(TopicIncoming::Edit {
            topic,
            tg_message: TgMessageId(50),
            text: EDIT_BODY.into(),
        })
        .await
        .unwrap();
    adapter.set_send_errors(1);
    bridge
        .handle_topic_message(TopicIncoming::Message {
            topic,
            tg_message: TgMessageId(51),
            text: "lost".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap();
    assert_eq!(sink.media().len(), 6);
    assert_eq!(adapter.sent().len(), 2);
}

async fn deliver_despite_count_failure<St>(
    bridge: &Bridge<FakeSink, St, ManualClock>,
    sink: &FakeSink,
    store: &St,
) where
    St: BridgeStore + 'static,
{
    let failures = store.failures().await.unwrap().len();
    let texts = sink.texts().len();
    bridge
        .handle_remote(counted_message("m-after", BODY, Vec::new()))
        .await
        .unwrap();
    assert_eq!(sink.texts().len(), texts + 1);
    assert!(
        store
            .message_for_network(&zalo_dm("a"), "m-after")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(store.failures().await.unwrap().len(), failures);
    assert!(
        store
            .message_for_network(&zalo_dm("a"), "m-fail")
            .await
            .unwrap()
            .is_none()
    );
}

fn expected_counts() -> Vec<CountedDelivery> {
    let row = |direction, kind, at| CountedDelivery {
        conversation: zalo_dm("a"),
        direction,
        kind,
        at_unix_secs: at,
    };
    vec![
        row(Direction::In, ContentKind::Text, SENT_AT),
        row(Direction::In, ContentKind::Photo, SENT_AT),
        row(Direction::In, ContentKind::Photo, SENT_AT),
        row(Direction::In, ContentKind::Photo, SENT_AT),
        row(Direction::In, ContentKind::Photo, SENT_AT),
        row(Direction::In, ContentKind::Voice, SENT_AT),
        row(Direction::In, ContentKind::Sticker, SENT_AT),
        row(Direction::Out, ContentKind::Text, OUT_AT),
    ]
}

fn bucket(at: i64) -> (i64, u8) {
    let day = at.div_euclid(hub_store::limits::SECS_PER_DAY);
    let hour = at
        .div_euclid(hub_store::limits::SECS_PER_HOUR)
        .rem_euclid(24);
    (day, u8::try_from(hour).unwrap())
}

fn assert_sqlite_buckets(store: &SqliteStore) {
    let rows = store
        .hub()
        .lock()
        .reader()
        .counters_between_days(0, 100_000)
        .unwrap();
    let dumped = format!("{rows:?}");
    assert!(!dumped.contains(BODY), "{dumped}");
    assert!(!dumped.contains(OUT_BODY), "{dumped}");
    assert!(!dumped.contains(EDIT_BODY), "{dumped}");
    let (day, hour) = bucket(SENT_AT);
    let (out_day, out_hour) = bucket(OUT_AT);
    let (clock_day, clock_hour) = bucket(START_UNIX_SECS);
    let count = |direction, kind, row_day, row_hour| -> u64 {
        rows.iter()
            .filter(|row| {
                row.direction == direction
                    && row.content_kind == kind
                    && row.day == row_day
                    && row.hour == row_hour
            })
            .map(|row| row.count)
            .sum()
    };
    assert_eq!(count(Direction::In, ContentKind::Text, day, hour), 1);
    assert_eq!(count(Direction::In, ContentKind::Photo, day, hour), 4);
    assert_eq!(count(Direction::In, ContentKind::Voice, day, hour), 1);
    assert_eq!(count(Direction::In, ContentKind::Sticker, day, hour), 1);
    assert_eq!(
        count(Direction::Out, ContentKind::Text, out_day, out_hour),
        1
    );
    assert_eq!(
        count(Direction::In, ContentKind::Text, clock_day, clock_hour),
        0
    );
    assert_eq!(rows.iter().map(|row| row.count).sum::<u64>(), 8);
}

fn counter_columns(path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let sql = String::from_utf8_lossy(&bytes);
    let start = sql.find("CREATE TABLE message_counters").unwrap();
    let rest = &sql[start..];
    let end = rest.find("PRIMARY KEY").unwrap();
    rest[..end]
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim().trim_matches(|c: char| c == '(' || c == ')');
            let name = trimmed.split_whitespace().next()?;
            if name.eq_ignore_ascii_case("create")
                || name.eq_ignore_ascii_case("primary")
                || name.eq_ignore_ascii_case("constraint")
            {
                return None;
            }
            Some(name.trim_matches(',').to_owned())
        })
        .collect()
}

fn assert_file_lacks(path: &std::path::Path, secret: &str) {
    let needle = secret.as_bytes();
    let main = std::fs::read(path).unwrap();
    assert!(
        !main.windows(needle.len()).any(|window| window == needle),
        "database stored message text"
    );
    for suffix in ["-wal", "-shm"] {
        let side = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        let Ok(bytes) = std::fs::read(&side) else {
            continue;
        };
        assert!(
            !bytes.windows(needle.len()).any(|window| window == needle),
            "{side:?} stored message text"
        );
    }
}

struct CountDir(std::path::PathBuf);

impl CountDir {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("hub-bridge-count-{}-{n}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> std::path::PathBuf {
        self.0.join("inbox.sqlite")
    }
}

impl Drop for CountDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn memory_store_counts_only_delivered_messages() {
    let h = harness();
    exercise_counts(&h.bridge, &h.sink, &h.adapter, &h.clock).await;
    let counts = h.store.counted();
    assert_eq!(counts, expected_counts());
    let dumped = format!("{counts:?}");
    assert!(!dumped.contains(BODY));
    assert!(!dumped.contains(OUT_BODY));
    assert!(!dumped.contains(EDIT_BODY));
    h.store.fail_counts(true);
    deliver_despite_count_failure(&h.bridge, &h.sink, &h.store).await;
    assert_eq!(h.store.counted(), counts);
}

#[tokio::test]
async fn sqlite_store_counts_only_delivered_messages() {
    let tmp = CountDir::new();
    let path = tmp.file();
    let sink = FakeSink::new();
    let store = SqliteStore::open(&path).unwrap();
    let clock = ManualClock::new(START_UNIX_SECS);
    let adapter = FakeAdapter::new(Network::Zalo);
    let bridge = Bridge::new(
        BridgeConfig::default(),
        vec![Box::new(adapter.clone())],
        sink.clone(),
        store.clone(),
        clock.clone(),
    );
    exercise_counts(&bridge, &sink, &adapter, &clock).await;
    assert_sqlite_buckets(&store);
    store.fail_counts(true);
    deliver_despite_count_failure(&bridge, &sink, &store).await;
    assert_sqlite_buckets(&store);
    drop(bridge);
    drop(store);
    assert_eq!(
        counter_columns(&path),
        [
            "day",
            "hour",
            "conversation_id",
            "direction",
            "content_kind",
            "count",
        ]
    );
    for secret in [BODY, OUT_BODY, EDIT_BODY] {
        assert_file_lacks(&path, secret);
    }
}
