//! [`crate::BridgeStore`] backed by [`hub_store::SharedHub`].
//!
//! Each call runs on `spawn_blocking`. `rusqlite::Connection` is not `Sync`, and the mutex in
//! [`hub_store::SharedHub`] is the share handle. A worker thread would only add a channel in
//! front of that same mutex.

use std::path::Path;

use hub_core::{ConversationId, MessageRef, TgMessageId, TopicId};
use hub_store::{NewFailure, SharedHub, StoredFailure, StoredTopic};

use crate::error::{Error, Result};
use crate::limits::AVATAR_CARD_SETTING_PREFIX;
use crate::mention::normalize_name;
use crate::store::{BridgeStore, FailureRecord, TopicMapping, scope};

const STORE_TASK_STOPPED: &str = "store task stopped";
const TOPIC_RANGE: &str = "topic id is out of range";
const MESSAGE_RANGE: &str = "telegram message id is out of range";
const MESSAGE_OWNED: &str = "network message id is owned by another conversation";

/// Bridge maps stored in Captain's `SQLite` file.
///
/// Cloning shares the connection. A new [`crate::Bridge`] with a store opened on the same
/// file is a restart: topic and message rows are still there.
#[derive(Clone, Debug)]
pub struct SqliteStore {
    hub: SharedHub,
}

impl SqliteStore {
    /// Wraps an open hub.
    pub fn new(hub: SharedHub) -> Self {
        Self { hub }
    }

    /// Opens `path` with [`SharedHub::open`](hub_store::SharedHub::open).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] when the file cannot be opened or migrated.
    pub fn open(path: &Path) -> Result<Self> {
        SharedHub::open(path)
            .map(Self::new)
            .map_err(|err| Error::Store {
                detail: clip(&err.to_string()),
            })
    }

    /// The shared connection. Callers that are not the bridge use this for health rows.
    pub fn hub(&self) -> &SharedHub {
        &self.hub
    }
}

impl BridgeStore for SqliteStore {
    async fn load_topics(&self) -> Result<Vec<TopicMapping>> {
        let hub = self.hub.clone();
        call(hub, |hub| {
            let mut out = Vec::new();
            for row in hub.topic_rows()? {
                out.push(mapping_from(row)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_topic(&self, mapping: TopicMapping) -> Result<()> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.upsert_topic_row(
                &mapping.conversation,
                i64::from(mapping.topic.0),
                &mapping.title,
                0,
            )?;
            Ok(())
        })
        .await
    }

    async fn remove_topic(&self, conversation: &ConversationId) -> Result<()> {
        let conversation = conversation.clone();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.delete_topic_row(&conversation)?;
            Ok(())
        })
        .await
    }

    async fn topic_for(&self, conversation: &ConversationId) -> Result<Option<TopicMapping>> {
        let conversation = conversation.clone();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.topic_row(&conversation)?.map(mapping_from).transpose()
        })
        .await
    }

    async fn conversation_for_topic(&self, topic: TopicId) -> Result<Option<ConversationId>> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.conversation_for_topic(i64::from(topic.0))
        })
        .await
    }

    async fn set_title(&self, conversation: &ConversationId, title: &str) -> Result<()> {
        let conversation = conversation.clone();
        let title = title.to_owned();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.ensure_conversation(&conversation, Some(&title), 0)?;
            Ok(())
        })
        .await
    }

    async fn title(&self, conversation: &ConversationId) -> Result<Option<String>> {
        let conversation = conversation.clone();
        let hub = self.hub.clone();
        call(hub, move |hub| hub.conversation_title(&conversation)).await
    }

    async fn message_for_network(
        &self,
        conversation: &ConversationId,
        message_id: &str,
    ) -> Result<Option<TgMessageId>> {
        let conversation = conversation.clone();
        let message_id = message_id.to_owned();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            match hub.message_link_for_network(&conversation, &message_id)? {
                Some(id) => Ok(Some(tg_id(id)?)),
                None => Ok(None),
            }
        })
        .await
    }

    async fn message_for_telegram(&self, tg_message: TgMessageId) -> Result<Option<MessageRef>> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            let Some(link) = hub.message_link_for_telegram(i64::from(tg_message.0))? else {
                return Ok(None);
            };
            let Some(conversation) = link.conversation else {
                return Ok(None);
            };
            Ok(Some(MessageRef {
                conversation,
                message_id: link.network_message_id,
            }))
        })
        .await
    }

    async fn insert_message(
        &self,
        conversation: &ConversationId,
        network_message_id: &str,
        tg_message: TgMessageId,
        at_unix_secs: i64,
    ) -> Result<()> {
        let conversation = conversation.clone();
        let network_message_id = network_message_id.to_owned();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            let stored = hub.insert_message_link(
                &conversation,
                &network_message_id,
                i64::from(tg_message.0),
                at_unix_secs,
            )?;
            if stored {
                Ok(())
            } else {
                Err(hub_store::Error::Corrupt(MESSAGE_OWNED.to_owned()))
            }
        })
        .await
    }

    async fn remove_message(&self, tg_message: TgMessageId) -> Result<()> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.delete_message_link(i64::from(tg_message.0))?;
            Ok(())
        })
        .await
    }

    async fn remove_conversation_messages(&self, conversation: &ConversationId) -> Result<()> {
        let conversation = conversation.clone();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.delete_conversation_links(&conversation)?;
            Ok(())
        })
        .await
    }

    async fn remember_user(
        &self,
        conversation: &ConversationId,
        user_id: &str,
        display_name: &str,
    ) -> Result<()> {
        let scope_key = scope(conversation);
        let name_key = normalize_name(display_name);
        let user_id = user_id.to_owned();
        let display_name = display_name.to_owned();
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.remember_user_name(&scope_key, &name_key, &user_id, &display_name)?;
            Ok(())
        })
        .await
    }

    async fn lookup_user(
        &self,
        conversation: &ConversationId,
        display_name: &str,
    ) -> Result<Option<String>> {
        let scope_key = scope(conversation);
        let name_key = normalize_name(display_name);
        let hub = self.hub.clone();
        call(hub, move |hub| hub.lookup_user_name(&scope_key, &name_key)).await
    }

    async fn record_failure(&self, failure: FailureRecord) -> Result<()> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            let conversation_id = match &failure.conversation {
                Some(id) => {
                    Some(hub.ensure_conversation(id, None, failure.created_at_unix_secs)?)
                }
                None => None,
            };
            hub.record_failure(&NewFailure {
                conversation_id,
                reason: failure.reason,
                retryable: failure.retryable,
                created_at_unix_secs: failure.created_at_unix_secs,
            })?;
            Ok(())
        })
        .await
    }

    async fn failures(&self) -> Result<Vec<FailureRecord>> {
        let hub = self.hub.clone();
        call(hub, |hub| {
            Ok(hub
                .stored_failures()?
                .into_iter()
                .map(failure_from)
                .collect())
        })
        .await
    }

    async fn record_retry(&self, at_unix_secs: i64) -> Result<()> {
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.record_retry_at(at_unix_secs)?;
            Ok(())
        })
        .await
    }

    async fn retry_marks_since(&self, since_unix_secs: i64) -> Result<Vec<i64>> {
        let hub = self.hub.clone();
        call(hub, move |hub| hub.retries_since(since_unix_secs)).await
    }

    async fn avatar_posted(&self, conversation: &ConversationId) -> Result<bool> {
        let key = avatar_key(conversation);
        let hub = self.hub.clone();
        call(hub, move |hub| {
            Ok(hub.setting(&key)?.as_deref() == Some("1"))
        })
        .await
    }

    async fn mark_avatar_posted(&self, conversation: &ConversationId) -> Result<()> {
        let key = avatar_key(conversation);
        let hub = self.hub.clone();
        call(hub, move |hub| {
            hub.set_setting(&key, "1")?;
            Ok(())
        })
        .await
    }
}

fn avatar_key(conversation: &ConversationId) -> String {
    format!("{AVATAR_CARD_SETTING_PREFIX}{conversation}")
}

fn mapping_from(row: StoredTopic) -> hub_store::Result<TopicMapping> {
    let topic =
        i32::try_from(row.topic_id).map_err(|_| hub_store::Error::Corrupt(TOPIC_RANGE.into()))?;
    Ok(TopicMapping {
        conversation: row.conversation,
        topic: TopicId(topic),
        title: row.title,
    })
}

fn tg_id(id: i64) -> hub_store::Result<TgMessageId> {
    i32::try_from(id)
        .map(TgMessageId)
        .map_err(|_| hub_store::Error::Corrupt(MESSAGE_RANGE.into()))
}

fn failure_from(row: StoredFailure) -> FailureRecord {
    FailureRecord {
        conversation: row.conversation,
        reason: row.reason,
        retryable: row.retryable,
        created_at_unix_secs: row.created_at_unix_secs,
    }
}

async fn call<T, F>(hub: SharedHub, op: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut hub_store::Hub) -> hub_store::Result<T> + Send + 'static,
{
    let joined = tokio::task::spawn_blocking(move || {
        let mut guard = hub.lock();
        op(&mut guard)
    })
    .await
    .map_err(|_| Error::Store {
        detail: STORE_TASK_STOPPED.to_owned(),
    })?;
    joined.map_err(|err| Error::Store {
        detail: clip(&err.to_string()),
    })
}

fn clip(detail: &str) -> String {
    const MAX: usize = 240;
    if detail.chars().count() <= MAX {
        return detail.to_owned();
    }
    detail.chars().take(MAX).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use hub_core::{ConversationKind, Network};

    use super::*;

    fn scratch() -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "hub-bridge-sqlite-{}-{nanos}-{n}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn new() -> Self {
            Self(scratch())
        }

        fn file(&self) -> std::path::PathBuf {
            self.0.join("inbox.sqlite")
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn group(id: &str) -> ConversationId {
        ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Group,
            external_id: id.to_owned(),
        }
    }

    #[tokio::test]
    async fn restart_restores_topics_messages_and_names() {
        let tmp = Tmp::new();
        let path = tmp.file();
        let conversation = group("room");
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .upsert_topic(TopicMapping {
                    conversation: conversation.clone(),
                    topic: TopicId(9),
                    title: "Room".into(),
                })
                .await
                .unwrap();
            store
                .insert_message(&conversation, "m1", TgMessageId(4), 10)
                .await
                .unwrap();
            store
                .insert_message(&conversation, "m1", TgMessageId(5), 11)
                .await
                .unwrap();
            store
                .remember_user(&conversation, "u1", "An")
                .await
                .unwrap();
            store.record_retry(20).await.unwrap();
        }
        let store = SqliteStore::open(&path).unwrap();
        let topics = store.load_topics().await.unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].topic, TopicId(9));
        assert_eq!(topics[0].title, "Room");
        assert_eq!(
            store.topic_for(&conversation).await.unwrap().unwrap().topic,
            TopicId(9)
        );
        assert_eq!(
            store.conversation_for_topic(TopicId(9)).await.unwrap(),
            Some(conversation.clone())
        );
        assert_eq!(
            store
                .message_for_network(&conversation, "m1")
                .await
                .unwrap(),
            Some(TgMessageId(4))
        );
        let reference = store
            .message_for_telegram(TgMessageId(4))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reference.message_id, "m1");
        assert_eq!(
            store
                .lookup_user(&conversation, "an")
                .await
                .unwrap()
                .as_deref(),
            Some("u1")
        );
        assert_eq!(store.retry_marks_since(20).await.unwrap(), vec![20]);
        store
            .remove_conversation_messages(&conversation)
            .await
            .unwrap();
        assert_eq!(
            store
                .message_for_network(&conversation, "m1")
                .await
                .unwrap(),
            None
        );
        assert!(store.topic_for(&conversation).await.unwrap().is_some());
        store.remove_topic(&conversation).await.unwrap();
        assert!(store.topic_for(&conversation).await.unwrap().is_none());
    }
}
