//! The inbox pipeline: scope gate, triggers, coalescing, triage and tickets.
//!
//! One task owns the coalescer, so events are handled strictly in arrival order. It learns
//! about updates from the connection task through a channel and never touches the update
//! stream itself.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use captain_core::{ChatId, ChatKind, EventKind, InboundEvent, MessageId, MessageView};
use captain_inbox::manual::{ManualCollector, ManualRequest, Pointer};
use captain_inbox::report::SELF_HANDLED;
use captain_inbox::{
    Arrival, Burst, Coalescer, Route, ThreadKey, TicketState, TriageFacts, Trigger, TriggerKind,
    triage, trigger,
};
use captain_store::{MetricEvent, MetricKind, NewTicket};
use tokio::sync::{mpsc, oneshot};

use crate::app::{App, now_unix};
use crate::telemetry::record_error;

const TICK: Duration = Duration::from_secs(1);
const SECS_PER_HOUR: i64 = 3600;
const SECS_PER_DAY: i64 = 86_400;
const MANUAL_PROFILE: &str = "manual";

/// Work for the pipeline task.
pub enum PipelineInput {
    /// An update from Telegram.
    Event(Box<InboundEvent>),
    /// A manual ticket from the CLI or the web UI.
    Manual {
        pointers: Vec<Pointer>,
        note: Option<String>,
        reply: Option<oneshot::Sender<Result<i64>>>,
    },
}

pub struct Pipeline {
    app: Arc<App>,
    coalescer: Coalescer,
    manual: ManualCollector,
}

impl Pipeline {
    pub fn new(app: Arc<App>) -> Self {
        let policy = app.config().policy;
        Self {
            app,
            coalescer: Coalescer::default(),
            manual: ManualCollector::new(policy.manual_before_secs, policy.manual_after_secs),
        }
    }

    /// Runs until the input channel closes. Errors are logged per event, so one bad update
    /// cannot stop the pipeline; a store failure is logged loudly each time it happens.
    pub async fn run(mut self, mut input: mpsc::UnboundedReceiver<PipelineInput>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                next = input.recv() => match next {
                    Some(PipelineInput::Event(event)) => {
                        if let Err(e) = self.on_event(*event).await {
                            tracing::error!(error = %e, "pipeline could not handle an update");
                        }
                    }
                    Some(PipelineInput::Manual { pointers, note, reply }) => {
                        let request = ManualRequest { note: note.unwrap_or_default(), pointers, at: now_unix() };
                        let result = self.manual_ticket(request).await;
                        if let Err(e) = &result {
                            tracing::error!(error = %e, "could not create a manual ticket");
                        }
                        if let Some(reply) = reply {
                            let _ = reply.send(result);
                        }
                    }
                    None => break,
                },
                _ = tick.tick() => {
                    if let Err(e) = self.on_tick(now_unix()).await {
                        tracing::error!(error = %e, "pipeline tick failed");
                    }
                }
            }
        }
        // Open bursts are dropped on shutdown: their messages are stored, and the next run's
        // recall metric counts any the owner then answered.
        tracing::info!(open = self.coalescer.open_count(), "pipeline stopped");
    }

    async fn on_tick(&mut self, now: i64) -> Result<()> {
        for burst in self.coalescer.due(now) {
            self.finish_burst(burst, now).await?;
        }
        if let Some(request) = self.manual.due(now) {
            self.manual_ticket(request).await?;
        }
        self.publish_open();
        Ok(())
    }

    fn publish_open(&self) {
        if let Some(live) = &self.app.live {
            live.open_bursts
                .store(self.coalescer.open_count(), Ordering::Relaxed);
        }
    }

    async fn on_event(&mut self, event: InboundEvent) -> Result<()> {
        let now = event.received_at_unix;
        match event.kind {
            EventKind::MessageNew(m) => self.on_message(m, now).await,
            EventKind::MessageEdited(m) => {
                let key = key_of(&m);
                if self.app.config().scope.lookup(key).is_some() {
                    self.app.db.run(move |s| s.edit_message(&m)).await?;
                }
                Ok(())
            }
            EventKind::MessagesDeleted { chat, ids } => {
                self.app
                    .db
                    .run(move |s| s.delete_messages(chat, &ids, now))
                    .await?;
                Ok(())
            }
            EventKind::ReadInbox {
                chat,
                topic,
                max_id,
                still_unread,
            } => {
                if self.app.config().scope.chat(chat).is_none() {
                    return Ok(());
                }
                let e = MetricEvent {
                    chat: Some(chat),
                    topic,
                    a: Some(i64::from(max_id.0)),
                    b: still_unread.map(i64::from),
                    ..MetricEvent::new(MetricKind::ReadAdvanced, now)
                };
                self.app.db.run(move |s| s.record(&e)).await
            }
            EventKind::Other => Ok(()),
        }
    }

    async fn on_message(&mut self, m: MessageView, now: i64) -> Result<()> {
        let Some(live) = &self.app.live else {
            return Ok(());
        };
        let self_id = live.handle.self_id();
        if m.date_unix < self.app.started_at {
            live.caught_up.fetch_add(1, Ordering::Relaxed);
        }
        if m.outgoing && m.chat.kind == ChatKind::User && m.chat.id != self_id {
            let e = MetricEvent {
                chat: Some(m.chat.id),
                ..MetricEvent::new(MetricKind::OutgoingDm, m.date_unix)
            };
            self.app.db.run(move |s| s.record(&e)).await?;
        }
        if m.chat.id == self_id {
            // Saved Messages: only `cap:` notes and their forwards are read, nothing is stored.
            if m.outgoing
                && let Some(request) = self.manual.observe(&m, now)
            {
                self.manual_ticket(request).await?;
            }
            return Ok(());
        }
        if crate::runner::handle_review_reply(&self.app, &m).await? {
            return Ok(());
        }

        let config = self.app.config();
        let key = key_of(&m);
        let Some(entry) = config.scope.lookup(key).cloned() else {
            if !m.outgoing && config.policy.count_out_of_scope {
                let is_bot = m.sender.as_ref().is_some_and(|s| s.is_bot);
                let would_trigger = m.mentioned || (m.chat.kind == ChatKind::User && !is_bot);
                let chat = m.chat.clone();
                self.app
                    .db
                    .run(move |s| s.count_gate_drop(&chat, would_trigger, now))
                    .await?;
            }
            return Ok(());
        };

        let live_message = m.date_unix >= self.app.started_at;
        let stored = m.clone();
        let chat_entry = entry.clone();
        let fresh = self
            .app
            .db
            .run(move |s| {
                // Catch-up after a crash replays what arrived since the last saved position.
                // A message already stored has been through the pipeline once.
                if s.message(stored.chat.id, stored.id)?.is_some() {
                    return Ok(false);
                }
                s.upsert_chat(&stored.chat, Some(&chat_entry), now)?;
                if let Some(sender) = &stored.sender
                    && sender.id != stored.chat.id
                {
                    s.upsert_chat(sender, None, now)?;
                }
                s.put_message(&stored, now, live_message)?;
                Ok(true)
            })
            .await?;
        if !fresh {
            tracing::debug!(
                chat = m.chat.id.0,
                message = m.id.0,
                "replayed message skipped"
            );
            return Ok(());
        }

        if m.outgoing {
            if let Some(burst) = self.coalescer.outgoing(key) {
                self.self_handled(burst, now).await?;
            }
            return Ok(());
        }
        if m.sender.as_ref().is_some_and(|s| s.is_bot) {
            return Ok(());
        }

        for burst in self.coalescer.due(now) {
            self.finish_burst(burst, now).await?;
        }
        let target_outgoing = self.reply_target_outgoing(&m, &entry, now).await?;
        let fired = trigger::detect(&m, &entry, target_outgoing);
        let arrival = Arrival {
            key,
            id: m.id,
            album: m.album_id,
            sender: m.sender.as_ref().map(|s| s.id),
            trigger: fired,
        };
        let limits = config.policy.profile(&entry.profile);
        if let Some(full) = self.coalescer.observe(arrival, &entry.profile, limits, now) {
            self.finish_burst(full, now).await?;
        }
        self.publish_open();
        Ok(())
    }

    /// Whether the message this one replies to is the account's, looking in the store first
    /// and asking Telegram only when the reply trigger needs it and the store does not know.
    async fn reply_target_outgoing(
        &self,
        m: &MessageView,
        entry: &captain_inbox::ChatEntry,
        now: i64,
    ) -> Result<Option<bool>> {
        let Some(target) = m.reply_to else {
            return Ok(None);
        };
        if !entry.enables(TriggerKind::ReplyToMe) || m.mentioned {
            return Ok(None);
        }
        let chat = m.chat.id;
        if let Some(known) = self
            .app
            .db
            .run(move |s| s.is_outgoing(chat, target))
            .await?
        {
            return Ok(Some(known));
        }
        let Some(live) = &self.app.live else {
            return Ok(None);
        };
        match live.handle.message(chat, target).await {
            Ok(found) => {
                let e = MetricEvent {
                    chat: Some(chat),
                    a: found.as_ref().map(|t| now - t.date_unix),
                    tag: Some(if found.is_some() { "hit" } else { "miss" }.into()),
                    ..MetricEvent::new(MetricKind::ReplyTargetFetch, now)
                };
                self.app.db.run(move |s| s.record(&e)).await?;
                Ok(found.map(|t| t.outgoing))
            }
            Err(e) => {
                record_error(&self.app.db, &e, now).await;
                Ok(None)
            }
        }
    }

    async fn finish_burst(&mut self, burst: Burst, now: i64) -> Result<()> {
        let config = self.app.config();
        let entry = config.scope.chat(burst.key.chat).cloned();
        let anchor = burst.anchor();
        let chat = burst.key.chat;
        let anchor_date = self
            .app
            .db
            .run(move |s| s.message(chat, anchor))
            .await?
            .map_or(burst.last_at, |m| m.date);
        let facts = TriageFacts {
            paused: self.app.paused.load(Ordering::Relaxed),
            muted: entry.as_ref().is_some_and(|e| e.muted(now)),
            anchor_age_secs: now - anchor_date,
        };
        let outcome = triage(&burst, facts, &config.policy);
        let trigger = burst.trigger().clone();
        let ticket = NewTicket {
            key: burst.key,
            chat_label: entry
                .as_ref()
                .map_or_else(|| chat.0.to_string(), |e| e.label.clone()),
            chat_kind: entry.as_ref().map(|e| e.kind),
            profile: burst.profile.clone(),
            trigger: trigger.kind,
            trigger_reason: trigger.reason,
            route: outcome.route,
            state: outcome.state,
            state_reason: outcome.reason.map(str::to_owned),
            stale: outcome.stale,
            note: None,
            opened_at: burst.opened_at,
            closed_at: now,
            anchor,
            anchor_date,
            messages: burst.messages.clone(),
            senders: burst.senders.clone(),
        };
        let key = burst.key;
        let id = self
            .app
            .db
            .run(move |s| {
                let id = s.create_ticket(&ticket)?;
                let (hops, oldest) = s.reply_chain(key.chat, anchor)?;
                let prior_hour = s.count_on_key(key, anchor_date - SECS_PER_HOUR, anchor_date)?;
                let prior_day = s.count_on_key(key, anchor_date - SECS_PER_DAY, anchor_date)?;
                s.record(&MetricEvent {
                    chat: Some(key.chat),
                    topic: key.topic,
                    ticket: Some(id),
                    a: Some(i64::try_from(hops).unwrap_or(i64::MAX)),
                    b: oldest.map(|o| anchor_date - o),
                    ..MetricEvent::new(MetricKind::ContextShape, now)
                })?;
                s.record(&MetricEvent {
                    chat: Some(key.chat),
                    topic: key.topic,
                    ticket: Some(id),
                    a: Some(prior_hour),
                    b: Some(prior_day),
                    ..MetricEvent::new(MetricKind::ContextDensity, now)
                })?;
                Ok(id)
            })
            .await?;
        tracing::info!(
            ticket = id,
            chat = chat.0,
            trigger = %trigger.kind,
            state = %outcome.state,
            messages = burst.messages.len(),
            "ticket"
        );
        Ok(())
    }

    async fn self_handled(&self, burst: Burst, now: i64) -> Result<()> {
        let config = self.app.config();
        let entry = config.scope.chat(burst.key.chat).cloned();
        let trigger: Trigger = burst.trigger().clone();
        let anchor = burst.anchor();
        let ticket = NewTicket {
            key: burst.key,
            chat_label: entry
                .as_ref()
                .map_or_else(|| burst.key.chat.0.to_string(), |e| e.label.clone()),
            chat_kind: entry.as_ref().map(|e| e.kind),
            profile: burst.profile.clone(),
            trigger: trigger.kind,
            trigger_reason: trigger.reason,
            route: None,
            state: TicketState::Skipped,
            state_reason: Some(SELF_HANDLED.into()),
            stale: false,
            note: None,
            opened_at: burst.opened_at,
            closed_at: now,
            anchor,
            anchor_date: burst.opened_at,
            messages: burst.messages,
            senders: burst.senders,
        };
        self.app.db.run(move |s| s.create_ticket(&ticket)).await?;
        Ok(())
    }

    /// Creates a manual ticket. Manual tickets bypass scope and pause: the owner asked.
    async fn manual_ticket(&self, request: ManualRequest) -> Result<i64> {
        let Some(last) = request.pointers.last().copied() else {
            anyhow::bail!("a manual ticket needs at least one message");
        };
        let chat = last.chat;
        let entry = self.app.config().scope.chat(chat).cloned();
        let messages: Vec<MessageId> = request
            .pointers
            .iter()
            .filter(|p| p.chat == chat)
            .map(|p| p.message)
            .collect();
        let note = (!request.note.is_empty()).then_some(request.note);
        let now = now_unix();
        let id = self
            .app
            .db
            .run(move |s| {
                let label = match &entry {
                    Some(e) => e.label.clone(),
                    None => chat_title(s, chat)?,
                };
                s.create_ticket(&NewTicket {
                    key: ThreadKey { chat, topic: None },
                    chat_label: label,
                    chat_kind: entry.as_ref().map(|e| e.kind),
                    profile: MANUAL_PROFILE.into(),
                    trigger: TriggerKind::Manual,
                    trigger_reason: format!("manual request for message {}", last.message.0),
                    route: Some(Route::Draft),
                    state: TicketState::Queued,
                    state_reason: None,
                    stale: false,
                    note,
                    opened_at: request.at,
                    closed_at: now,
                    anchor: last.message,
                    anchor_date: request.at,
                    messages,
                    senders: Vec::new(),
                })
            })
            .await?;
        tracing::info!(ticket = id, chat = chat.0, "manual ticket");
        Ok(id)
    }
}

fn chat_title(s: &captain_store::Store, chat: ChatId) -> captain_store::Result<String> {
    Ok(s.chat_title(chat)?.unwrap_or_else(|| chat.0.to_string()))
}

fn key_of(m: &MessageView) -> ThreadKey {
    ThreadKey {
        chat: m.chat.id,
        topic: m.topic,
    }
}
