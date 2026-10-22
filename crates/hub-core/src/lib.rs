//! Source-agnostic vocabulary shared by every hub crate.
//!
//! This crate owns networks, conversation ids, remote events, and the two traits the bridge
//! drives: [`NetworkAdapter`] for a source network and [`TopicSink`] for Telegram forum topics.
//! It does no I/O.
//!
//! Conversation ids use `network:kind:external_id`. See [`ConversationId`].

pub mod adapter;
pub mod error;
pub mod event;
pub mod health;
pub mod ids;
pub mod limits;
pub mod outbound;
pub mod sink;

pub use adapter::NetworkAdapter;
pub use error::{Error, Result};
pub use event::{
    Attachment, AttachmentKind, AttachmentSource, Mention, RemoteEdit, RemoteEvent, RemoteMessage,
};
pub use health::{ModuleHealth, ModuleState, SessionState};
pub use ids::{ConversationId, ConversationKind, MessageRef, Network, TgMessageId, TopicId};
pub use outbound::OutgoingMessage;
pub use sink::TopicSink;

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::future::Future;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    use super::*;

    const RETRY_AFTER_SECS: u64 = 2;
    const SAMPLE_UNIX_SECS: i64 = 1_700_000_000;

    struct FakeAdapter {
        incoming: Mutex<VecDeque<RemoteEvent>>,
        sent: Mutex<Vec<OutgoingMessage>>,
        reactions: Mutex<Vec<(MessageRef, String, bool)>>,
        unsent: Mutex<Vec<MessageRef>>,
    }

    impl FakeAdapter {
        fn new(events: Vec<RemoteEvent>) -> Self {
            Self {
                incoming: Mutex::new(VecDeque::from(events)),
                sent: Mutex::new(Vec::new()),
                reactions: Mutex::new(Vec::new()),
                unsent: Mutex::new(Vec::new()),
            }
        }
    }

    fn lock<'a, T>(mutex: &'a Mutex<T>) -> Result<std::sync::MutexGuard<'a, T>> {
        mutex.lock().map_err(|_| Error::Adapter {
            detail: "test lock poisoned".into(),
        })
    }

    impl NetworkAdapter for FakeAdapter {
        fn network(&self) -> Network {
            Network::Zalo
        }

        async fn next_event(&self) -> Result<Option<RemoteEvent>> {
            Ok(lock(&self.incoming)?.pop_front())
        }

        async fn send(&self, message: OutgoingMessage) -> Result<MessageRef> {
            let conversation = message.conversation.clone();
            lock(&self.sent)?.push(message);
            Ok(MessageRef {
                conversation,
                message_id: "sent-1".into(),
            })
        }

        async fn react(&self, target: MessageRef, emoji: String, added: bool) -> Result<()> {
            lock(&self.reactions)?.push((target, emoji, added));
            Ok(())
        }

        async fn unsend(&self, target: MessageRef) -> Result<()> {
            lock(&self.unsent)?.push(target);
            Ok(())
        }

        async fn health(&self) -> Result<ModuleHealth> {
            Ok(ModuleHealth {
                module: "zalo".into(),
                state: ModuleState::Ok,
                detail: String::new(),
                updated_at_unix_secs: SAMPLE_UNIX_SECS,
            })
        }
    }

    struct FakeSink {
        open: Mutex<HashSet<i32>>,
        next_topic: Mutex<i32>,
        next_message: Mutex<i32>,
        create_rate_limits_left: Mutex<u32>,
        send_rate_limits_left: Mutex<u32>,
        titles: Mutex<Vec<String>>,
        texts: Mutex<Vec<(TopicId, String, Option<TgMessageId>)>>,
        media: Mutex<Vec<(TopicId, AttachmentKind, Option<String>)>>,
        reactions: Mutex<Vec<(TopicId, TgMessageId, Option<String>)>>,
        deleted: Mutex<Vec<(TopicId, TgMessageId)>>,
    }

    impl FakeSink {
        fn new() -> Self {
            Self {
                open: Mutex::new(HashSet::new()),
                next_topic: Mutex::new(0),
                next_message: Mutex::new(100),
                create_rate_limits_left: Mutex::new(1),
                send_rate_limits_left: Mutex::new(1),
                titles: Mutex::new(Vec::new()),
                texts: Mutex::new(Vec::new()),
                media: Mutex::new(Vec::new()),
                reactions: Mutex::new(Vec::new()),
                deleted: Mutex::new(Vec::new()),
            }
        }

        fn take_rate_limit(&self, left: &Mutex<u32>) -> Result<bool> {
            let mut left = lock(left)?;
            if *left == 0 {
                return Ok(false);
            }
            *left -= 1;
            Ok(true)
        }

        fn require_open(&self, topic: TopicId) -> Result<()> {
            if lock(&self.open)?.contains(&topic.0) {
                Ok(())
            } else {
                Err(Error::TopicGone)
            }
        }

        fn next_tg_message(&self) -> Result<TgMessageId> {
            let mut next = lock(&self.next_message)?;
            *next += 1;
            Ok(TgMessageId(*next))
        }
    }

    impl TopicSink for FakeSink {
        async fn create_topic(&self, title: String) -> Result<TopicId> {
            if self.take_rate_limit(&self.create_rate_limits_left)? {
                return Err(Error::RateLimited {
                    retry_after_secs: RETRY_AFTER_SECS,
                });
            }
            let mut next = lock(&self.next_topic)?;
            *next += 1;
            let topic = TopicId(*next);
            lock(&self.open)?.insert(topic.0);
            lock(&self.titles)?.push(title);
            Ok(topic)
        }

        async fn close_topic(&self, topic: TopicId) -> Result<()> {
            if !lock(&self.open)?.remove(&topic.0) {
                return Err(Error::TopicGone);
            }
            Ok(())
        }

        async fn send_text(
            &self,
            topic: TopicId,
            text_html: String,
            reply_to: Option<TgMessageId>,
        ) -> Result<TgMessageId> {
            self.require_open(topic)?;
            if self.take_rate_limit(&self.send_rate_limits_left)? {
                return Err(Error::RateLimited {
                    retry_after_secs: RETRY_AFTER_SECS,
                });
            }
            let id = self.next_tg_message()?;
            lock(&self.texts)?.push((topic, text_html, reply_to));
            Ok(id)
        }

        async fn send_media(
            &self,
            topic: TopicId,
            attachment: Attachment,
            caption: Option<String>,
        ) -> Result<TgMessageId> {
            self.require_open(topic)?;
            let id = self.next_tg_message()?;
            lock(&self.media)?.push((topic, attachment.kind, caption));
            Ok(id)
        }

        async fn set_reaction(
            &self,
            topic: TopicId,
            tg_message: TgMessageId,
            emoji: Option<String>,
        ) -> Result<()> {
            self.require_open(topic)?;
            lock(&self.reactions)?.push((topic, tg_message, emoji));
            Ok(())
        }

        async fn delete_message(&self, topic: TopicId, tg_message: TgMessageId) -> Result<()> {
            self.require_open(topic)?;
            lock(&self.deleted)?.push((topic, tg_message));
            Ok(())
        }
    }

    fn poll<F: Future>(future: F) -> std::result::Result<F::Output, &'static str> {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        // Fakes never await, so the first poll finishes. A pending future is a test failure.
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => Ok(value),
            Poll::Pending => Err("future stayed pending"),
        }
    }

    fn assert_send<T: Send>(_: &T) {}

    fn assert_futures_are_send(adapter: &FakeAdapter, sink: &FakeSink) {
        {
            let pending = adapter.next_event();
            assert_send(&pending);
        }
        {
            let pending = adapter.send(OutgoingMessage {
                conversation: ConversationId {
                    network: Network::Zalo,
                    kind: ConversationKind::Dm,
                    external_id: "user-1".into(),
                },
                text: String::new(),
                attachments: Vec::new(),
                reply_to: None,
            });
            assert_send(&pending);
        }
        {
            let pending = sink.create_topic("title".into());
            assert_send(&pending);
        }
        {
            let pending = sink.send_text(TopicId(1), "<b>hi</b>".into(), None);
            assert_send(&pending);
        }
    }

    async fn drive(adapter: &FakeAdapter, sink: &FakeSink) -> Result<()> {
        assert_eq!(adapter.network(), Network::Zalo);
        assert!(matches!(
            sink.close_topic(TopicId(0)).await,
            Err(Error::TopicGone)
        ));

        let event = adapter.next_event().await?;
        let RemoteEvent::Message(incoming) = event.ok_or_else(|| Error::Adapter {
            detail: "expected a message".into(),
        })?
        else {
            return Err(Error::Adapter {
                detail: "expected a message".into(),
            });
        };

        assert!(matches!(
            sink.create_topic(incoming.sender_display_name.clone()).await,
            Err(Error::RateLimited { retry_after_secs }) if retry_after_secs == RETRY_AFTER_SECS
        ));
        let topic = sink.create_topic(incoming.sender_display_name).await?;
        assert!(matches!(
            sink.send_text(topic, "<b>hello</b>".into(), None).await,
            Err(Error::RateLimited { retry_after_secs }) if retry_after_secs == RETRY_AFTER_SECS
        ));
        let posted = sink
            .send_text(topic, "<b>hello</b>".into(), Some(TgMessageId(7)))
            .await?;
        let attachment = incoming
            .attachments
            .first()
            .cloned()
            .ok_or_else(|| Error::Adapter {
                detail: "expected an attachment".into(),
            })?;
        let media_id = sink
            .send_media(topic, attachment, Some("cap".into()))
            .await?;
        sink.set_reaction(topic, posted, Some("heart".into()))
            .await?;
        sink.set_reaction(topic, posted, None).await?;
        sink.delete_message(topic, media_id).await?;

        let target = MessageRef {
            conversation: incoming.conversation.clone(),
            message_id: incoming.message_id.clone(),
        };
        let sent = adapter
            .send(OutgoingMessage {
                conversation: incoming.conversation,
                text: "pong".into(),
                attachments: Vec::new(),
                reply_to: Some(target.clone()),
            })
            .await?;
        assert_eq!(sent.message_id, "sent-1");
        adapter.react(target.clone(), "heart".into(), true).await?;
        adapter.unsend(target).await?;

        let health = adapter.health().await?;
        assert_eq!(health.state, ModuleState::Ok);
        assert_eq!(health.module, "zalo");
        assert!(adapter.next_event().await?.is_none());

        sink.close_topic(topic).await?;
        assert!(matches!(
            sink.close_topic(topic).await,
            Err(Error::TopicGone)
        ));
        assert!(matches!(
            sink.send_text(topic, "late".into(), None).await,
            Err(Error::TopicGone)
        ));
        Ok(())
    }

    #[test]
    fn default_rename_is_unsupported() -> std::result::Result<(), String> {
        let sink = FakeSink::new();
        let result = poll(sink.rename_topic(TopicId(1), "Room".into()))?;
        assert_eq!(result, Err(Error::Unsupported));
        Ok(())
    }

    #[test]
    fn bridge_drives_a_fake_adapter_and_sink() -> std::result::Result<(), String> {
        let adapter = FakeAdapter::new(vec![RemoteEvent::Message(RemoteMessage {
            conversation: ConversationId {
                network: Network::Zalo,
                kind: ConversationKind::Dm,
                external_id: "user-1".into(),
            },
            message_id: "m-1".into(),
            sender_id: "user-1".into(),
            sender_display_name: "An".into(),
            text: "hello".into(),
            attachments: vec![Attachment {
                kind: AttachmentKind::Photo,
                mime: Some("image/jpeg".into()),
                size_bytes: Some(12),
                source: AttachmentSource::Url("https://cdn.example/a.jpg".into()),
                file_name: Some("a.jpg".into()),
            }],
            reply_to: None,
            sent_at_unix_secs: SAMPLE_UNIX_SECS,
            mentions: Vec::new(),
        })]);
        let sink = FakeSink::new();
        assert_futures_are_send(&adapter, &sink);
        poll(drive(&adapter, &sink))?.map_err(|error| error.to_string())?;

        let titles = lock(&sink.titles).map_err(|error| error.to_string())?;
        assert_eq!(titles.as_slice(), ["An"]);
        let texts = lock(&sink.texts).map_err(|error| error.to_string())?;
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0].1, "<b>hello</b>");
        assert_eq!(texts[0].2, Some(TgMessageId(7)));
        let media = lock(&sink.media).map_err(|error| error.to_string())?;
        assert_eq!(media[0].1, AttachmentKind::Photo);
        assert_eq!(media[0].2.as_deref(), Some("cap"));
        let reactions = lock(&sink.reactions).map_err(|error| error.to_string())?;
        assert_eq!(reactions[0].2.as_deref(), Some("heart"));
        assert_eq!(reactions[1].2, None);
        let deleted = lock(&sink.deleted).map_err(|error| error.to_string())?;
        assert_eq!(deleted.len(), 1);
        let sent = lock(&adapter.sent).map_err(|error| error.to_string())?;
        assert_eq!(sent[0].text, "pong");
        assert_eq!(
            lock(&adapter.reactions)
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        assert_eq!(
            lock(&adapter.unsent)
                .map_err(|error| error.to_string())?
                .len(),
            1
        );
        Ok(())
    }
}
