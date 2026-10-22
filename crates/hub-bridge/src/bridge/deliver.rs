//! How one event becomes sink calls or an adapter send.
//!
//! A topic is created only after `create_topic` succeeds. On [`hub_core::Error::TopicGone`] the
//! stale mapping is dropped before a replacement is opened, and that replacement happens at most
//! [`TOPIC_RECREATE_LIMIT`] times. A delivery error is stored and the lane moves on.

use std::collections::HashMap;

use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, ConversationKind, DirectoryTitle,
    Mention, MessageRef, OutgoingMessage, RemoteEdit, RemoteEvent, RemoteMessage, SessionState,
    TgMessageId, TopicId,
};

use crate::error::Result;
use crate::format::{notice_html, render_caption, render_text_html, topic_title};
use crate::incoming::TopicIncoming;
use crate::limits::{
    AVATAR_CARD_FILE, FALLBACK_TOPIC_NAME, MENTION_MAX_WORDS, RENAME_INTERVAL_SECS,
    SINK_MAX_ATTEMPTS, TOPIC_RECREATE_LIMIT,
};
use crate::mention::{ResolvedMention, is_mention_at, next_prefix};
use crate::reaction::{to_network_reaction, to_telegram_reaction};
use crate::store::TopicMapping;

use super::{Inner, Plan, SinkOp, SinkOut, Step, cache_insert, lock, topic_of};

type CoreError = hub_core::Error;

fn is_placeholder(title: &str, external_id: &str) -> bool {
    let title = title.trim();
    title.is_empty() || title == external_id
}

impl<S, St, C> Inner<S, St, C>
where
    S: hub_core::TopicSink + 'static,
    St: crate::store::BridgeStore + 'static,
    C: crate::clock::Clock + 'static,
{
    pub(super) async fn conversation_for(&self, topic: TopicId) -> Result<Option<ConversationId>> {
        if let Some(conversation) = lock(&self.cache).conversations.get(&topic).cloned() {
            return Ok(Some(conversation));
        }
        let Some(conversation) = self.store.conversation_for_topic(topic).await? else {
            return Ok(None);
        };
        {
            let mut cache = lock(&self.cache);
            cache.topics.insert(conversation.clone(), topic);
            cache.conversations.insert(topic, conversation.clone());
        }
        Ok(Some(conversation))
    }

    pub(super) async fn dispatch_remote(&self, event: RemoteEvent) -> Result<()> {
        match event {
            RemoteEvent::Message(message) => self.on_message(message).await,
            RemoteEvent::Edit(edit) => self.on_edit(edit).await,
            RemoteEvent::Unsend { target } => self.on_unsend(&target).await,
            RemoteEvent::Reaction {
                target,
                emoji,
                added,
            } => self.on_reaction(target, &emoji, added).await,
            RemoteEvent::ChatInfo {
                conversation,
                title,
                avatar,
            } => self.on_chat_info(conversation, title, avatar).await,
            RemoteEvent::GroupEvent {
                conversation,
                text,
                sent_at_unix_secs: _,
            } => self.on_group(&conversation, &text).await,
            RemoteEvent::FriendRequest {
                sender_id,
                sender_display_name,
                message,
            } => {
                self.on_friend(&sender_id, &sender_display_name, &message)
                    .await
            }
            RemoteEvent::SessionChanged { state } => self.on_session(state).await,
            RemoteEvent::DirectorySnapshot { entries } => self.on_directory(entries).await,
        }
    }

    pub(super) async fn dispatch_incoming(&self, incoming: TopicIncoming) -> Result<()> {
        let topic = topic_of(&incoming);
        let Some(conversation) = self.conversation_for(topic).await? else {
            self.record_unmapped(topic).await?;
            return Ok(());
        };
        match incoming {
            TopicIncoming::Message {
                tg_message,
                text,
                attachments,
                reply_to,
                topic: _,
            } => {
                self.on_topic_message(&conversation, tg_message, text, attachments, reply_to)
                    .await
            }
            TopicIncoming::Reaction {
                tg_message,
                emoji,
                added,
                topic: _,
            } => {
                self.on_topic_reaction(&conversation, tg_message, &emoji, added)
                    .await
            }
            TopicIncoming::Edit {
                tg_message,
                text,
                topic: _,
            } => self.on_topic_edit(&conversation, tg_message, text).await,
            TopicIncoming::Unsend {
                tg_message,
                topic: _,
            } => self.on_topic_unsend(&conversation, tg_message).await,
        }
    }

    async fn on_message(&self, message: RemoteMessage) -> Result<()> {
        if self
            .store
            .message_for_network(&message.conversation, &message.message_id)
            .await?
            .is_some()
        {
            tracing::debug!(
                conversation = %message.conversation,
                message_id = %message.message_id,
                "duplicate remote message skipped"
            );
            return Ok(());
        }
        self.remember_names(&message).await?;
        if message.text.trim().is_empty() && message.attachments.is_empty() {
            return Ok(());
        }
        let reply = self.reply_tg(&message).await?;
        let plans = plans_for(&message, reply);
        let Some(tg) = self
            .deliver_plans(&message.conversation, &message.sender_display_name, plans)
            .await?
        else {
            return Ok(());
        };
        self.store
            .insert_message(
                &message.conversation,
                &message.message_id,
                tg,
                self.clock.unix_secs(),
            )
            .await
    }

    async fn on_edit(&self, edit: RemoteEdit) -> Result<()> {
        if edit.text.trim().is_empty() && edit.attachments.is_empty() {
            return Ok(());
        }
        let conversation = edit.target.conversation.clone();
        // No sink edit call exists. A known target becomes a follow-up that replies to the
        // mapped post. The original row stays so later reactions still find it.
        let reply = self
            .store
            .message_for_network(&conversation, &edit.target.message_id)
            .await?;
        let plans = plans_for_body("edited", &edit.text, &[], edit.attachments, reply);
        self.deliver_plans(&conversation, "", plans).await?;
        Ok(())
    }

    async fn on_unsend(&self, target: &MessageRef) -> Result<()> {
        let Some(tg) = self
            .store
            .message_for_network(&target.conversation, &target.message_id)
            .await?
        else {
            return self
                .plain_notice(&target.conversation, "message removed")
                .await;
        };
        let Some(topic) = self.topic_id(&target.conversation).await? else {
            return self
                .plain_notice(&target.conversation, "message removed")
                .await;
        };
        match self.sink_call(SinkOp::Delete { topic, tg }).await {
            Step::Done(_) => self.store.remove_message(tg).await,
            Step::Sink(CoreError::TopicGone) => {
                self.forget_topic(&target.conversation).await?;
                Ok(())
            }
            Step::Sink(error) => {
                self.record_core(Some(target.conversation.clone()), &error)
                    .await?;
                Ok(())
            }
        }
    }

    async fn on_reaction(&self, target: MessageRef, emoji: &str, added: bool) -> Result<()> {
        let Some(tg) = self
            .store
            .message_for_network(&target.conversation, &target.message_id)
            .await?
        else {
            return self
                .plain_notice(&target.conversation, &format!("reaction {emoji}"))
                .await;
        };
        let Some(topic) = self.topic_id(&target.conversation).await? else {
            return self
                .plain_notice(&target.conversation, &format!("reaction {emoji}"))
                .await;
        };
        let mapped = if added {
            let Some(emoji) = to_telegram_reaction(emoji) else {
                tracing::debug!(conversation = %target.conversation, "reaction skipped");
                return Ok(());
            };
            Some(emoji.to_owned())
        } else {
            None
        };
        match self
            .sink_call(SinkOp::React {
                topic,
                tg,
                emoji: mapped,
            })
            .await
        {
            Step::Done(_) => Ok(()),
            Step::Sink(CoreError::TopicGone) => {
                self.forget_topic(&target.conversation).await?;
                self.record_core(Some(target.conversation), &CoreError::TopicGone)
                    .await?;
                Ok(())
            }
            Step::Sink(error) => {
                self.record_core(Some(target.conversation), &error).await?;
                Ok(())
            }
        }
    }

    async fn on_directory(&self, entries: Vec<DirectoryTitle>) -> Result<()> {
        let _pass = self.backfill.lock().await;
        let mut names = HashMap::with_capacity(entries.len());
        for entry in entries {
            let title = entry.title.trim();
            if title.is_empty() || title == entry.conversation.external_id {
                continue;
            }
            names.insert(entry.conversation, title.to_owned());
        }
        let stored = self.store.load_topics().await?;
        let mut pending = Vec::new();
        for mapping in stored {
            let Some(name) = names.get(&mapping.conversation) else {
                continue;
            };
            let current = lock(&self.cache)
                .titles
                .get(&mapping.conversation)
                .cloned()
                .unwrap_or(mapping.title);
            if !is_placeholder(&current, &mapping.conversation.external_id) {
                continue;
            }
            pending.push((mapping.topic, mapping.conversation, name.clone()));
        }
        pending.sort_by_key(|(topic, _, _)| topic.0);
        let waiting = pending.len();
        let cap = self.config.backfill_rename_cap;
        let mut started = 0usize;
        for (_topic, conversation, name) in pending {
            if started >= cap {
                break;
            }
            // Seed `applied` from the stored id before the write. A cold cache would
            // otherwise treat the name we are about to store as the last title set.
            if self.topic_id(&conversation).await?.is_none() {
                continue;
            }
            if !self.backfill_should_rename(&conversation, &name) {
                continue;
            }
            if started > 0 && !self.backfill_gap().await {
                break;
            }
            self.store.set_title(&conversation, &name).await?;
            lock(&self.cache).titles.insert(conversation.clone(), name);
            self.rename_if_needed(&conversation).await?;
            started += 1;
        }
        tracing::info!(renames = started, waiting, "directory backfill");
        Ok(())
    }

    async fn backfill_gap(&self) -> bool {
        let gap = self.config.backfill_rename_gap_secs;
        if gap <= 0 {
            return !*self.stop.borrow();
        }
        let deadline = self.clock.unix_secs().saturating_add(gap);
        let sleep = self.clock.sleep_until(deadline);
        tokio::pin!(sleep);
        let mut stop = self.stop.subscribe();
        if *stop.borrow() {
            return false;
        }
        tokio::select! {
            biased;
            changed = stop.changed() => changed.is_ok() && !*stop.borrow(),
            () = &mut sleep => !*self.stop.borrow(),
        }
    }

    fn backfill_should_rename(&self, conversation: &ConversationId, name: &str) -> bool {
        let cache = lock(&self.cache);
        let applied = cache
            .applied
            .get(conversation)
            .map(String::as_str)
            .unwrap_or("");
        let rendered = topic_title(conversation.network, conversation.kind, name);
        let previous = topic_title(conversation.network, conversation.kind, applied);
        if rendered == previous {
            return false;
        }
        if let Some(at) = cache.renamed_at.get(conversation).copied()
            && self.clock.unix_secs().saturating_sub(at) < RENAME_INTERVAL_SECS
        {
            return false;
        }
        true
    }

    async fn on_chat_info(
        &self,
        conversation: ConversationId,
        title: Option<String>,
        avatar: Option<AttachmentSource>,
    ) -> Result<()> {
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            self.store.set_title(&conversation, &title).await?;
            lock(&self.cache).titles.insert(conversation.clone(), title);
        }
        if let Some(avatar) = avatar {
            lock(&self.cache)
                .avatars
                .insert(conversation.clone(), avatar);
        }
        self.rename_if_needed(&conversation).await?;
        if let Some(topic) = self.topic_id(&conversation).await? {
            self.post_avatar_card(&conversation, topic).await;
        }
        Ok(())
    }

    async fn on_group(&self, conversation: &ConversationId, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let plans = vec![Plan::Text {
            html: notice_html(text),
            reply: None,
        }];
        self.deliver_plans(conversation, "", plans).await?;
        Ok(())
    }

    async fn on_friend(
        &self,
        sender_id: &str,
        sender_display_name: &str,
        message: &str,
    ) -> Result<()> {
        let name = if sender_display_name.trim().is_empty() {
            sender_id
        } else {
            sender_display_name
        };
        let body = if message.trim().is_empty() {
            format!("friend request from {name} ({sender_id})")
        } else {
            format!("friend request from {name} ({sender_id}): {message}")
        };
        self.on_ops_notice(&body).await
    }

    async fn on_session(&self, state: SessionState) -> Result<()> {
        self.on_ops_notice(&format!("session {}", session_label(state)))
            .await
    }

    async fn on_ops_notice(&self, text: &str) -> Result<()> {
        let Some(topic) = self.config.ops_topic else {
            tracing::info!("ops event has no topic");
            return Ok(());
        };
        match self
            .sink_call(SinkOp::Text {
                topic,
                html: notice_html(text),
                reply: None,
            })
            .await
        {
            Step::Done(_) => Ok(()),
            Step::Sink(error) => {
                self.record_core(None, &error).await?;
                Ok(())
            }
        }
    }

    async fn on_topic_message(
        &self,
        conversation: &ConversationId,
        tg_message: TgMessageId,
        text: String,
        attachments: Vec<Attachment>,
        reply_to: Option<TgMessageId>,
    ) -> Result<()> {
        let resolved = self.resolved_mentions(conversation, &text).await?;
        tracing::trace!(resolved = resolved.len(), "topic mentions resolved");
        let reply_to = match reply_to {
            Some(tg) => self.store.message_for_telegram(tg).await?,
            None => None,
        };
        let Some(reference) = self
            .send_outgoing(conversation, text, attachments, reply_to)
            .await?
        else {
            return Ok(());
        };
        self.store
            .insert_message(
                conversation,
                &reference.message_id,
                tg_message,
                self.clock.unix_secs(),
            )
            .await
    }

    async fn on_topic_reaction(
        &self,
        conversation: &ConversationId,
        tg_message: TgMessageId,
        emoji: &str,
        added: bool,
    ) -> Result<()> {
        let Some(target) = self.store.message_for_telegram(tg_message).await? else {
            return self.send_plain(conversation, emoji).await;
        };
        let network_emoji = if added {
            match to_network_reaction(conversation.network, emoji) {
                Some(emoji) => emoji,
                None => {
                    tracing::debug!(conversation = %conversation, "topic reaction skipped");
                    return Ok(());
                }
            }
        } else {
            to_network_reaction(conversation.network, emoji).unwrap_or_else(|| emoji.to_owned())
        };
        let Some(adapter) = self.adapter_for(conversation.network) else {
            return self.missing_adapter(conversation).await;
        };
        if let Err(error) = adapter.react(target, network_emoji, added).await {
            self.record_core(Some(conversation.clone()), &error).await?;
        }
        Ok(())
    }

    async fn on_topic_edit(
        &self,
        conversation: &ConversationId,
        tg_message: TgMessageId,
        text: String,
    ) -> Result<()> {
        let reply_to = self.store.message_for_telegram(tg_message).await?;
        self.send_outgoing(conversation, text, Vec::new(), reply_to)
            .await?;
        Ok(())
    }

    async fn on_topic_unsend(
        &self,
        conversation: &ConversationId,
        tg_message: TgMessageId,
    ) -> Result<()> {
        let Some(target) = self.store.message_for_telegram(tg_message).await? else {
            return self.send_plain(conversation, "message removed").await;
        };
        let Some(adapter) = self.adapter_for(conversation.network) else {
            return self.missing_adapter(conversation).await;
        };
        match adapter.unsend(target).await {
            Ok(()) => self.store.remove_message(tg_message).await,
            Err(error) => {
                self.record_core(Some(conversation.clone()), &error).await?;
                Ok(())
            }
        }
    }

    async fn deliver_plans(
        &self,
        conversation: &ConversationId,
        title_hint: &str,
        plans: Vec<Plan>,
    ) -> Result<Option<TgMessageId>> {
        if plans.is_empty() {
            return Ok(None);
        }
        match self
            .send_with_recovery(conversation, title_hint, &plans)
            .await?
        {
            Ok(id) => Ok(Some(id)),
            Err(error) => {
                self.record_core(Some(conversation.clone()), &error).await?;
                Ok(None)
            }
        }
    }

    async fn send_with_recovery(
        &self,
        conversation: &ConversationId,
        title_hint: &str,
        plans: &[Plan],
    ) -> Result<std::result::Result<TgMessageId, CoreError>> {
        let mut topic = match self.ensure_topic(conversation, title_hint).await? {
            Step::Done(topic) => topic,
            Step::Sink(error) => return Ok(Err(error)),
        };
        let mut replacements = 0;
        loop {
            match self.execute_plan(topic, plans).await? {
                Step::Done(id) => return Ok(Ok(id)),
                Step::Sink(CoreError::TopicGone) => {
                    // INVARIANT: drop the dead topic before create, so a failed create leaves no row.
                    self.forget_topic(conversation).await?;
                    if replacements >= TOPIC_RECREATE_LIMIT {
                        return Ok(Err(CoreError::TopicGone));
                    }
                    replacements += 1;
                    self.note_retry().await;
                    tracing::warn!(conversation = %conversation, "topic is gone, recreating");
                    topic = match self.create_topic(conversation, title_hint).await? {
                        Step::Done(topic) => topic,
                        Step::Sink(error) => return Ok(Err(error)),
                    };
                }
                Step::Sink(error) => return Ok(Err(error)),
            }
        }
    }

    async fn ensure_topic(
        &self,
        conversation: &ConversationId,
        hint: &str,
    ) -> Result<Step<TopicId>> {
        if let Some(topic) = self.topic_id(conversation).await? {
            return Ok(Step::Done(topic));
        }
        self.create_topic(conversation, hint).await
    }

    async fn create_topic(
        &self,
        conversation: &ConversationId,
        hint: &str,
    ) -> Result<Step<TopicId>> {
        let raw = self.title_for(conversation, hint).await?;
        let rendered = topic_title(conversation.network, conversation.kind, &raw);
        match self.sink_call(SinkOp::Create(rendered)).await {
            Step::Done(SinkOut::Topic(topic)) => {
                let mapping = TopicMapping {
                    conversation: conversation.clone(),
                    topic,
                    title: raw.clone(),
                };
                self.store.upsert_topic(mapping.clone()).await?;
                {
                    let mut cache = lock(&self.cache);
                    cache_insert(&mut cache, &mapping);
                    cache.applied.insert(conversation.clone(), raw);
                }
                tracing::info!(conversation = %conversation, topic = topic.0, "topic created");
                self.post_avatar_card(conversation, topic).await;
                Ok(Step::Done(topic))
            }
            Step::Done(_) => Ok(Step::Sink(CoreError::Adapter {
                detail: "sink returned an unexpected result".into(),
            })),
            Step::Sink(error) => Ok(Step::Sink(error)),
        }
    }

    async fn execute_plan(&self, topic: TopicId, plans: &[Plan]) -> Result<Step<TgMessageId>> {
        let mut first = None;
        for plan in plans {
            let op = match plan {
                Plan::Text { html, reply } => SinkOp::Text {
                    topic,
                    html: html.clone(),
                    reply: *reply,
                },
                Plan::Media {
                    attachments,
                    caption,
                } => {
                    let Some(attachment) = attachments.first().cloned() else {
                        continue;
                    };
                    SinkOp::Media {
                        topic,
                        attachment,
                        caption: caption.clone(),
                    }
                }
            };
            match self.sink_call(op).await {
                Step::Done(out) => match message_id(&out) {
                    Ok(id) => {
                        if first.is_none() {
                            first = Some(id);
                        }
                    }
                    Err(error) => return Ok(Step::Sink(error)),
                },
                Step::Sink(error) => return Ok(Step::Sink(error)),
            }
        }
        match first {
            Some(id) => Ok(Step::Done(id)),
            None => Ok(Step::Sink(CoreError::Adapter {
                detail: "empty send plan".into(),
            })),
        }
    }

    async fn sink_call(&self, op: SinkOp) -> Step<SinkOut> {
        let mut attempt = 0;
        loop {
            if !self.wait_rate_limit().await {
                return Step::Sink(CoreError::Adapter {
                    detail: "shut down during rate limit".into(),
                });
            }
            attempt += 1;
            match self.once(op.clone()).await {
                Ok(out) => return Step::Done(out),
                Err(CoreError::RateLimited { retry_after_secs }) if attempt < SINK_MAX_ATTEMPTS => {
                    self.pause(retry_after_secs);
                    self.note_retry().await;
                }
                Err(error) => return Step::Sink(error),
            }
        }
    }

    async fn once(&self, op: SinkOp) -> hub_core::Result<SinkOut> {
        match op {
            SinkOp::Create(title) => Ok(SinkOut::Topic(self.sink.create_topic(title).await?)),
            SinkOp::Text { topic, html, reply } => Ok(SinkOut::Message(
                self.sink.send_text(topic, html, reply).await?,
            )),
            SinkOp::Media {
                topic,
                attachment,
                caption,
            } => Ok(SinkOut::Message(
                self.sink.send_media(topic, attachment, caption).await?,
            )),
            SinkOp::React { topic, tg, emoji } => {
                self.sink.set_reaction(topic, tg, emoji).await?;
                Ok(SinkOut::Unit)
            }
            SinkOp::Delete { topic, tg } => {
                self.sink.delete_message(topic, tg).await?;
                Ok(SinkOut::Unit)
            }
            SinkOp::Rename { topic, title } => {
                self.sink.rename_topic(topic, title).await?;
                Ok(SinkOut::Unit)
            }
        }
    }

    async fn rename_if_needed(&self, conversation: &ConversationId) -> Result<()> {
        let Some(topic) = self.topic_id(conversation).await? else {
            return Ok(());
        };
        let desired = self.title_for(conversation, "").await?;
        let applied = lock(&self.cache)
            .applied
            .get(conversation)
            .cloned()
            .unwrap_or_default();
        let rendered = topic_title(conversation.network, conversation.kind, &desired);
        let previous = topic_title(conversation.network, conversation.kind, &applied);
        if rendered == previous {
            return Ok(());
        }
        let now = self.clock.unix_secs();
        if let Some(at) = lock(&self.cache).renamed_at.get(conversation).copied()
            && now.saturating_sub(at) < RENAME_INTERVAL_SECS
        {
            return Ok(());
        }
        match self
            .sink_call(SinkOp::Rename {
                topic,
                title: rendered,
            })
            .await
        {
            Step::Done(_) => {
                {
                    let mut cache = lock(&self.cache);
                    cache.applied.insert(conversation.clone(), desired.clone());
                    cache.renamed_at.insert(conversation.clone(), now);
                }
                self.store
                    .upsert_topic(TopicMapping {
                        conversation: conversation.clone(),
                        topic,
                        title: desired,
                    })
                    .await?;
                Ok(())
            }
            Step::Sink(CoreError::Unsupported) => {
                tracing::debug!(conversation = %conversation, "rename is unsupported");
                Ok(())
            }
            Step::Sink(CoreError::TopicGone) => {
                self.forget_topic(conversation).await?;
                tracing::warn!(conversation = %conversation, "rename found a missing topic");
                match self.create_topic(conversation, "").await? {
                    Step::Done(_) => Ok(()),
                    Step::Sink(error) => {
                        self.record_core(Some(conversation.clone()), &error).await?;
                        Ok(())
                    }
                }
            }
            Step::Sink(error) => {
                self.record_core(Some(conversation.clone()), &error).await?;
                Ok(())
            }
        }
    }

    async fn post_avatar_card(&self, conversation: &ConversationId, topic: TopicId) {
        if !self.config.avatar_card {
            return;
        }
        let Some(source) = lock(&self.cache).avatars.get(conversation).cloned() else {
            return;
        };
        match self.store.avatar_posted(conversation).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(conversation = %conversation, %error, "avatar flag was not read");
                return;
            }
        }
        let attachment = Attachment {
            kind: AttachmentKind::Photo,
            mime: None,
            size_bytes: None,
            source,
            file_name: Some(AVATAR_CARD_FILE.to_owned()),
        };
        match self
            .sink_call(SinkOp::Media {
                topic,
                attachment,
                caption: None,
            })
            .await
        {
            Step::Done(_) => {
                if let Err(error) = self.store.mark_avatar_posted(conversation).await {
                    tracing::warn!(conversation = %conversation, %error, "avatar flag was not stored");
                }
            }
            Step::Sink(error) => {
                tracing::warn!(conversation = %conversation, %error, "avatar card was not posted");
            }
        }
    }

    async fn topic_id(&self, conversation: &ConversationId) -> Result<Option<TopicId>> {
        if let Some(topic) = lock(&self.cache).topics.get(conversation).copied() {
            return Ok(Some(topic));
        }
        let Some(mapping) = self.store.topic_for(conversation).await? else {
            return Ok(None);
        };
        let topic = mapping.topic;
        cache_insert(&mut lock(&self.cache), &mapping);
        Ok(Some(topic))
    }

    async fn title_for(&self, conversation: &ConversationId, hint: &str) -> Result<String> {
        if let Some(title) = lock(&self.cache).titles.get(conversation).cloned()
            && !title.trim().is_empty()
        {
            return Ok(title);
        }
        if let Some(title) = self.store.title(conversation).await?
            && !title.trim().is_empty()
        {
            lock(&self.cache)
                .titles
                .insert(conversation.clone(), title.clone());
            return Ok(title);
        }
        if conversation.kind == ConversationKind::Dm {
            let trimmed = hint.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_owned());
            }
        }
        if !conversation.external_id.is_empty() {
            return Ok(conversation.external_id.clone());
        }
        Ok(FALLBACK_TOPIC_NAME.to_owned())
    }

    async fn forget_topic(&self, conversation: &ConversationId) -> Result<()> {
        {
            let mut cache = lock(&self.cache);
            if let Some(topic) = cache.topics.remove(conversation) {
                cache.conversations.remove(&topic);
            }
        }
        self.store.remove_topic(conversation).await?;
        self.store.remove_conversation_messages(conversation).await
    }

    async fn reply_tg(&self, message: &RemoteMessage) -> Result<Option<TgMessageId>> {
        let Some(reply) = &message.reply_to else {
            return Ok(None);
        };
        self.store
            .message_for_network(&reply.conversation, &reply.message_id)
            .await
    }

    async fn remember_names(&self, message: &RemoteMessage) -> Result<()> {
        self.store
            .remember_user(
                &message.conversation,
                &message.sender_id,
                &message.sender_display_name,
            )
            .await?;
        for mention in &message.mentions {
            self.store
                .remember_user(
                    &message.conversation,
                    &mention.user_id,
                    &mention.display_name,
                )
                .await?;
        }
        Ok(())
    }

    async fn plain_notice(&self, conversation: &ConversationId, text: &str) -> Result<()> {
        let plans = vec![Plan::Text {
            html: notice_html(text),
            reply: None,
        }];
        self.deliver_plans(conversation, "", plans).await?;
        Ok(())
    }

    async fn send_plain(&self, conversation: &ConversationId, text: &str) -> Result<()> {
        self.send_outgoing(conversation, text.to_owned(), Vec::new(), None)
            .await?;
        Ok(())
    }

    async fn send_outgoing(
        &self,
        conversation: &ConversationId,
        text: String,
        attachments: Vec<Attachment>,
        reply_to: Option<MessageRef>,
    ) -> Result<Option<MessageRef>> {
        let Some(adapter) = self.adapter_for(conversation.network) else {
            self.missing_adapter(conversation).await?;
            return Ok(None);
        };
        let message = OutgoingMessage {
            conversation: conversation.clone(),
            text,
            attachments,
            reply_to,
        };
        match adapter.send(message).await {
            Ok(reference) => Ok(Some(reference)),
            Err(error) => {
                self.record_core(Some(conversation.clone()), &error).await?;
                Ok(None)
            }
        }
    }

    async fn missing_adapter(&self, conversation: &ConversationId) -> Result<()> {
        self.record_core(
            Some(conversation.clone()),
            &CoreError::Adapter {
                detail: "no adapter for this network".into(),
            },
        )
        .await
    }

    async fn record_unmapped(&self, topic: TopicId) -> Result<()> {
        self.store
            .record_failure(crate::store::FailureRecord {
                conversation: None,
                reason: crate::store::clip_reason(&format!("unmapped topic {}", topic.0)),
                retryable: false,
                created_at_unix_secs: self.clock.unix_secs(),
            })
            .await?;
        self.open_failures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Same walk as [`crate::mention::resolve_mentions`], awaiting the store for each prefix.
    async fn resolved_mentions(
        &self,
        conversation: &ConversationId,
        text: &str,
    ) -> Result<Vec<ResolvedMention>> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        let mut index = 0;
        while index < chars.len() {
            if !is_mention_at(&chars, index) {
                index += 1;
                continue;
            }
            let mut cursor = index + 1;
            let mut best = None;
            for _ in 0..MENTION_MAX_WORDS {
                let Some((candidate, next)) = next_prefix(&chars, index + 1, cursor) else {
                    break;
                };
                cursor = next;
                if let Some(user_id) = self.store.lookup_user(conversation, &candidate).await? {
                    best = Some(ResolvedMention {
                        user_id,
                        display_name: candidate,
                    });
                }
            }
            if let Some(found) = best {
                index += 1 + found.display_name.chars().count();
                out.push(found);
            } else {
                index += 1;
            }
        }
        Ok(out)
    }
}

fn plans_for(message: &RemoteMessage, reply: Option<TgMessageId>) -> Vec<Plan> {
    plans_for_body(
        &message.sender_display_name,
        &message.text,
        &message.mentions,
        message.attachments.clone(),
        reply,
    )
}

fn plans_for_body(
    sender: &str,
    text: &str,
    mentions: &[Mention],
    attachments: Vec<Attachment>,
    reply: Option<TgMessageId>,
) -> Vec<Plan> {
    if attachments.is_empty() {
        return vec![Plan::Text {
            html: render_text_html(sender, text, mentions),
            reply,
        }];
    }
    // `send_media` has no reply slot. The caption carries the text. The reply is not sent.
    attachments
        .into_iter()
        .enumerate()
        .map(|(index, attachment)| Plan::Media {
            attachments: vec![attachment],
            caption: (index == 0).then(|| render_caption(sender, text)),
        })
        .collect()
}

fn message_id(out: &SinkOut) -> std::result::Result<TgMessageId, CoreError> {
    match out {
        SinkOut::Message(id) => Ok(*id),
        SinkOut::Topic(_) | SinkOut::Unit => Err(CoreError::Adapter {
            detail: "sink returned an unexpected result".into(),
        }),
    }
}

fn session_label(state: SessionState) -> &'static str {
    match state {
        SessionState::Valid => "valid",
        SessionState::Expired => "expired",
        SessionState::LoggedOut => "logged_out",
        SessionState::Connecting => "connecting",
    }
}
