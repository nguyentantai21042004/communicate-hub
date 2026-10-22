//! Persistence the bridge needs, independent of `hub-store`.
//!
//! `hub-store` can insert topic and message rows, but it cannot look them back up, drop a dead
//! topic, or remember a display name. [`BridgeStore`] is the contract this crate drives.
//! [`MemoryStore`] is the in-memory implementation tests and a process without `SQLite` use.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use hub_core::{ConversationId, ConversationKind, MessageRef, TgMessageId, TopicId};
use hub_store::{ContentKind, Direction};

use crate::error::Result;
use crate::mention::normalize_name;

/// One conversation's forum topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicMapping {
    /// Source conversation.
    pub conversation: ConversationId,
    /// Forum topic that mirrors it.
    pub topic: TopicId,
    /// Title last stored for that topic.
    pub title: String,
}

/// A delivery that did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRecord {
    /// Conversation involved, when the failure is about one.
    pub conversation: Option<ConversationId>,
    /// Why it failed. Already clipped.
    pub reason: String,
    /// Whether a later attempt might succeed.
    pub retryable: bool,
    /// When it failed, Unix seconds.
    pub created_at_unix_secs: i64,
}

/// Maps, names, and failure rows the bridge reads and writes.
///
/// Implementations must be safe to share across Tokio tasks. A method that cannot find a row
/// returns `Ok(None)` or `Ok(false)`, not an error.
pub trait BridgeStore: Send + Sync {
    /// Every stored topic mapping.
    ///
    /// Called once at startup, before adapters are subscribed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn load_topics(&self) -> impl Future<Output = Result<Vec<TopicMapping>>> + Send;

    /// Inserts or replaces the topic for `conversation`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn upsert_topic(&self, mapping: TopicMapping) -> impl Future<Output = Result<()>> + Send;

    /// Drops the topic mapping for `conversation`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn remove_topic(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Topic for `conversation`, if one is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn topic_for(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<Option<TopicMapping>>> + Send;

    /// Conversation that owns `topic`, if one is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn conversation_for_topic(
        &self,
        topic: TopicId,
    ) -> impl Future<Output = Result<Option<ConversationId>>> + Send;

    /// Remembers a display title before a topic exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn set_title(
        &self,
        conversation: &ConversationId,
        title: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Stored title, if one was set or a topic was upserted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn title(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Telegram message a network message was delivered as.
    ///
    /// The key is the conversation plus the network message id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn message_for_network(
        &self,
        conversation: &ConversationId,
        message_id: &str,
    ) -> impl Future<Output = Result<Option<TgMessageId>>> + Send;

    /// Network message a Telegram message was bridged from or to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn message_for_telegram(
        &self,
        tg_message: TgMessageId,
    ) -> impl Future<Output = Result<Option<MessageRef>>> + Send;

    /// Records that `network_message_id` was delivered as `tg_message`.
    ///
    /// The first write wins. A second write for the same network message does not replace it,
    /// so only the first Telegram message of a media group keeps the id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn insert_message(
        &self,
        conversation: &ConversationId,
        network_message_id: &str,
        tg_message: TgMessageId,
        at_unix_secs: i64,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Forgets the mapping for a Telegram message and its network id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn remove_message(&self, tg_message: TgMessageId) -> impl Future<Output = Result<()>> + Send;

    /// Forgets every message mapping in `conversation`.
    ///
    /// Called when that conversation's topic is dropped. The Telegram ids belonged to the
    /// deleted topic and must not be reused as reply targets.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn remove_conversation_messages(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Remembers `display_name` for `user_id` inside `conversation`'s scope.
    ///
    /// Groups are scoped to that conversation. Direct messages are scoped to the network.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn remember_user(
        &self,
        conversation: &ConversationId,
        user_id: &str,
        display_name: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// User id for a display name in `conversation`'s scope.
    ///
    /// Matching is case and diacritic insensitive.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn lookup_user(
        &self,
        conversation: &ConversationId,
        display_name: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Appends a failed delivery.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn record_failure(&self, failure: FailureRecord) -> impl Future<Output = Result<()>> + Send;

    /// Failures recorded so far, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn failures(&self) -> impl Future<Output = Result<Vec<FailureRecord>>> + Send;

    /// Records that a sink call was retried at `at_unix_secs`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn record_retry(&self, at_unix_secs: i64) -> impl Future<Output = Result<()>> + Send;

    /// Retry timestamps at or after `since_unix_secs`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn retry_marks_since(
        &self,
        since_unix_secs: i64,
    ) -> impl Future<Output = Result<Vec<i64>>> + Send;

    /// Whether the avatar card was already posted for `conversation`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the read fails.
    fn avatar_posted(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Records that the avatar card was posted for `conversation`.
    ///
    /// A second call leaves the flag set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the write fails.
    fn mark_avatar_posted(
        &self,
        conversation: &ConversationId,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Counts one delivered message in the conversation's UTC hour bucket.
    ///
    /// The default writes nothing, so a store that only keeps maps stays valid.
    /// `direction` is `in` for a remote message that reached Telegram and `out` for a topic
    /// message the adapter accepted. `at_unix_secs` is the event time when the event carries
    /// one, otherwise the bridge clock. Call this after the conversation row exists.
    /// A store error must leave the delivery result untouched.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the counter cannot be written. The default returns `Ok(())`.
    fn count_message(
        &self,
        conversation: &ConversationId,
        direction: Direction,
        kind: ContentKind,
        at_unix_secs: i64,
    ) -> impl Future<Output = Result<()>> + Send {
        let _ = (self, conversation, direction, kind, at_unix_secs);
        std::future::ready(Ok(()))
    }
}

#[derive(Debug, Default)]
struct MemoryInner {
    topics: HashMap<ConversationId, TopicMapping>,
    titles: HashMap<ConversationId, String>,
    by_network: HashMap<(ConversationId, String), TgMessageId>,
    by_telegram: HashMap<TgMessageId, MessageRef>,
    users: HashMap<(String, String), String>,
    failures: Vec<FailureRecord>,
    retries: Vec<i64>,
    restores: u64,
    avatar_posted: HashSet<ConversationId>,
    #[cfg(test)]
    counts: Vec<CountedDelivery>,
    #[cfg(test)]
    fail_count: bool,
    #[cfg(test)]
    lookups: Vec<(String, Option<String>)>,
}

/// One `count_message` call. The fields are the counter columns.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CountedDelivery {
    /// Conversation the delivery belongs to.
    pub conversation: ConversationId,
    /// In or out.
    pub direction: Direction,
    /// Payload kind.
    pub kind: ContentKind,
    /// Event time, or the bridge clock when the event has none.
    pub at_unix_secs: i64,
}

/// In-memory [`BridgeStore`].
///
/// Cloning shares the same maps. A restart in tests is a new [`crate::Bridge`] with the same
/// store, not a new store.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<Mutex<MemoryInner>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times [`BridgeStore::load_topics`] has run.
    pub fn restore_calls(&self) -> u64 {
        self.lock().restores
    }

    #[cfg(test)]
    pub(crate) fn lookup_log(&self) -> Vec<(String, Option<String>)> {
        self.lock().lookups.clone()
    }

    /// Counter rows recorded by [`BridgeStore::count_message`], in call order.
    #[cfg(test)]
    pub(crate) fn counted(&self) -> Vec<CountedDelivery> {
        self.lock().counts.clone()
    }

    #[cfg(test)]
    pub(crate) fn fail_counts(&self, fail: bool) {
        self.lock().fail_count = fail;
    }
}

impl MemoryStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        // A poisoned map still holds the rows a test needs to read.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl BridgeStore for MemoryStore {
    async fn load_topics(&self) -> Result<Vec<TopicMapping>> {
        let mut inner = self.lock();
        inner.restores += 1;
        Ok(inner.topics.values().cloned().collect())
    }

    async fn upsert_topic(&self, mapping: TopicMapping) -> Result<()> {
        let mut inner = self.lock();
        inner
            .titles
            .insert(mapping.conversation.clone(), mapping.title.clone());
        inner.topics.insert(mapping.conversation.clone(), mapping);
        Ok(())
    }

    async fn remove_topic(&self, conversation: &ConversationId) -> Result<()> {
        self.lock().topics.remove(conversation);
        Ok(())
    }

    async fn topic_for(&self, conversation: &ConversationId) -> Result<Option<TopicMapping>> {
        Ok(self.lock().topics.get(conversation).cloned())
    }

    async fn conversation_for_topic(&self, topic: TopicId) -> Result<Option<ConversationId>> {
        Ok(self
            .lock()
            .topics
            .values()
            .find(|mapping| mapping.topic == topic)
            .map(|mapping| mapping.conversation.clone()))
    }

    async fn set_title(&self, conversation: &ConversationId, title: &str) -> Result<()> {
        self.lock()
            .titles
            .insert(conversation.clone(), title.to_owned());
        Ok(())
    }

    async fn title(&self, conversation: &ConversationId) -> Result<Option<String>> {
        Ok(self.lock().titles.get(conversation).cloned())
    }

    async fn message_for_network(
        &self,
        conversation: &ConversationId,
        message_id: &str,
    ) -> Result<Option<TgMessageId>> {
        Ok(self
            .lock()
            .by_network
            .get(&(conversation.clone(), message_id.to_owned()))
            .copied())
    }

    async fn message_for_telegram(&self, tg_message: TgMessageId) -> Result<Option<MessageRef>> {
        Ok(self.lock().by_telegram.get(&tg_message).cloned())
    }

    async fn insert_message(
        &self,
        conversation: &ConversationId,
        network_message_id: &str,
        tg_message: TgMessageId,
        _at_unix_secs: i64,
    ) -> Result<()> {
        let mut inner = self.lock();
        let key = (conversation.clone(), network_message_id.to_owned());
        if inner.by_network.contains_key(&key) {
            return Ok(());
        }
        inner.by_network.insert(key, tg_message);
        inner.by_telegram.insert(
            tg_message,
            MessageRef {
                conversation: conversation.clone(),
                message_id: network_message_id.to_owned(),
            },
        );
        Ok(())
    }

    async fn remove_message(&self, tg_message: TgMessageId) -> Result<()> {
        let mut inner = self.lock();
        let Some(reference) = inner.by_telegram.remove(&tg_message) else {
            return Ok(());
        };
        inner
            .by_network
            .remove(&(reference.conversation, reference.message_id));
        Ok(())
    }

    async fn remove_conversation_messages(&self, conversation: &ConversationId) -> Result<()> {
        let mut inner = self.lock();
        inner.by_network.retain(|(key, _), _| key != conversation);
        inner
            .by_telegram
            .retain(|_, reference| &reference.conversation != conversation);
        Ok(())
    }

    async fn remember_user(
        &self,
        conversation: &ConversationId,
        user_id: &str,
        display_name: &str,
    ) -> Result<()> {
        let folded = normalize_name(display_name);
        if folded.is_empty() || user_id.is_empty() {
            return Ok(());
        }
        self.lock()
            .users
            .insert((scope(conversation), folded), user_id.to_owned());
        Ok(())
    }

    async fn lookup_user(
        &self,
        conversation: &ConversationId,
        display_name: &str,
    ) -> Result<Option<String>> {
        let key = (scope(conversation), normalize_name(display_name));
        let found = self.lock().users.get(&key).cloned();
        #[cfg(test)]
        self.lock().lookups.push((key.1, found.clone()));
        Ok(found)
    }

    async fn record_failure(&self, failure: FailureRecord) -> Result<()> {
        self.lock().failures.push(failure);
        Ok(())
    }

    async fn failures(&self) -> Result<Vec<FailureRecord>> {
        Ok(self.lock().failures.clone())
    }

    async fn record_retry(&self, at_unix_secs: i64) -> Result<()> {
        self.lock().retries.push(at_unix_secs);
        Ok(())
    }

    async fn retry_marks_since(&self, since_unix_secs: i64) -> Result<Vec<i64>> {
        Ok(self
            .lock()
            .retries
            .iter()
            .copied()
            .filter(|at| *at >= since_unix_secs)
            .collect())
    }

    async fn avatar_posted(&self, conversation: &ConversationId) -> Result<bool> {
        Ok(self.lock().avatar_posted.contains(conversation))
    }

    async fn mark_avatar_posted(&self, conversation: &ConversationId) -> Result<()> {
        self.lock().avatar_posted.insert(conversation.clone());
        Ok(())
    }

    async fn count_message(
        &self,
        conversation: &ConversationId,
        direction: Direction,
        kind: ContentKind,
        at_unix_secs: i64,
    ) -> Result<()> {
        #[cfg(test)]
        {
            let mut inner = self.lock();
            if inner.fail_count {
                return Err(crate::error::Error::Store {
                    detail: "count failed".into(),
                });
            }
            inner.counts.push(CountedDelivery {
                conversation: conversation.clone(),
                direction,
                kind,
                at_unix_secs,
            });
        }
        #[cfg(not(test))]
        let _ = (conversation, direction, kind, at_unix_secs);
        Ok(())
    }
}

pub(crate) fn scope(conversation: &ConversationId) -> String {
    if conversation.kind == ConversationKind::Group {
        conversation.to_string()
    } else {
        format!("network:{}", conversation.network)
    }
}

/// Clips `reason` to [`crate::limits::FAILURE_REASON_MAX_CHARS`] Unicode scalars.
pub(crate) fn clip_reason(reason: &str) -> String {
    let max = crate::limits::FAILURE_REASON_MAX_CHARS;
    let count = reason.chars().count();
    if count <= max {
        return reason.to_owned();
    }
    reason.chars().take(max).collect()
}
