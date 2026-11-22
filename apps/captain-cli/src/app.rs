//! State shared by the daemon's tasks, and the operations both the socket and the web UI offer.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use captain_core::{ChatSummary, InboundEvent};
use captain_inbox::{ExplicitLabel, Policy, Scope, TicketState, TriggerKind, report};
use captain_protocol::Handle;
use captain_store::{Store, TicketFilter};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

use crate::paths::Paths;
use crate::pipeline::PipelineInput;

const SECS_PER_DAY: i64 = 86_400;
const HISTORY_SCAN_CAP: usize = 20_000; // 200 getHistory pages; keeps one search from hammering the API
const PRIVATE_POST_URL: &str = "https://t.me/c"; // opens a channel message for its members
const SENDER_TICKET_CAP: usize = 10_000; // one pass over tickets when counting senders
const READ_BUSY_TIMEOUT: Duration = Duration::from_secs(5); // wait while the daemon holds the file

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The store behind a mutex, used from blocking threads.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Store>>);

impl Db {
    pub fn new(store: Store) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// Runs `f` on a blocking thread with the store.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> captain_store::Result<T> + Send + 'static,
    {
        let store = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let guard = store.lock().map_err(|_| anyhow!("store lock poisoned"))?;
            f(&guard).map_err(anyhow::Error::from)
        })
        .await
        .context("store task panicked")?
    }
}

/// One module log row. `detail` is not message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModuleLog {
    /// When it happened, Unix seconds.
    pub at_unix_secs: i64,
    /// Module name.
    pub module: String,
    /// Stored level name.
    pub level: String,
    /// Machine tag, for example `send_failed`.
    pub kind: String,
    /// Short note. Empty when the column is null.
    pub detail: String,
}

/// Inbound sender names and counts in `[since_unix_secs, until_unix_secs)`.
///
/// The store's message read includes text. This function drops that field and returns only
/// the display name and the count.
///
/// # Errors
///
/// Returns the store error when a ticket or its messages cannot be read.
pub(crate) fn sender_counts(
    store: &Store,
    since_unix_secs: i64,
    until_unix_secs: i64,
    limit: usize,
) -> captain_store::Result<Vec<(String, u64)>> {
    let tickets = store.tickets(&TicketFilter {
        since: Some(since_unix_secs),
        limit: Some(SENDER_TICKET_CAP),
        ..TicketFilter::default()
    })?;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for ticket in tickets {
        for message in store.ticket_message_bodies(ticket.id)? {
            if message.outgoing || message.date < since_unix_secs || message.date >= until_unix_secs
            {
                continue;
            }
            let Some(name) = message.sender_name.filter(|name| !name.is_empty()) else {
                continue;
            };
            *counts.entry(name).or_default() += 1;
        }
    }
    let mut rows: Vec<(String, u64)> = counts.into_iter().collect();
    rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    rows.truncate(limit);
    Ok(rows)
}

/// Newest module log rows, at most `limit`.
///
/// # Errors
///
/// Returns an error when the file cannot be read or a count does not fit in `u64`.
pub(crate) fn module_logs(path: &Path, limit: u32) -> Result<Vec<ModuleLog>> {
    let conn = open_read(path)?;
    let mut stmt = conn
        .prepare(
            "SELECT ts, module, level, kind, coalesce(detail, '')
             FROM module_events
             ORDER BY ts DESC, id DESC
             LIMIT ?1",
        )
        .context("module events")?;
    let rows = stmt
        .query_map([i64::from(limit)], |row| {
            Ok(ModuleLog {
                at_unix_secs: row.get(0)?,
                module: row.get(1)?,
                level: row.get(2)?,
                kind: row.get(3)?,
                detail: row.get(4)?,
            })
        })
        .context("module events")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("module events")
}

/// How many module log rows of `kind` fall in the half-open window.
///
/// `since_unix_secs` of `None` counts every row before `until_unix_secs`.
///
/// # Errors
///
/// Returns an error when the file cannot be read or the count does not fit in `u64`.
pub(crate) fn count_module_kind(
    path: &Path,
    kind: &str,
    since_unix_secs: Option<i64>,
    until_unix_secs: i64,
) -> Result<u64> {
    let conn = open_read(path)?;
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM module_events
             WHERE kind = ?1 AND ts < ?2 AND (?3 IS NULL OR ts >= ?3)",
            params![kind, until_unix_secs, since_unix_secs],
            |row| row.get(0),
        )
        .context("module event count")?;
    u64::try_from(count).context("module event count")
}

/// Detail of the newest module log row of `kind` in the half-open window.
///
/// # Errors
///
/// Returns an error when the file cannot be read.
pub(crate) fn latest_module_detail(
    path: &Path,
    kind: &str,
    since_unix_secs: Option<i64>,
    until_unix_secs: i64,
) -> Result<Option<String>> {
    let conn = open_read(path)?;
    conn.query_row(
        "SELECT detail FROM module_events
         WHERE kind = ?1 AND ts < ?2 AND (?3 IS NULL OR ts >= ?3)
         ORDER BY ts DESC, id DESC
         LIMIT 1",
        params![kind, until_unix_secs, since_unix_secs],
        |row| row.get(0),
    )
    .optional()
    .context("module event detail")
}

fn open_read(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot read {}", path.display()))?;
    conn.busy_timeout(READ_BUSY_TIMEOUT)
        .context("module event timeout")?;
    Ok(conn)
}

/// Scope and policy, replaced whole on reload.
#[derive(Debug, Clone)]
pub struct Config {
    pub scope: Arc<Scope>,
    pub policy: Arc<Policy>,
    pub scope_missing: bool,
}

impl Config {
    /// Reads `policy.toml` then `scope.toml`. A missing policy uses defaults; a missing scope
    /// watches nothing.
    pub fn load(paths: &Paths) -> Result<Self> {
        let policy = match read_optional(&paths.policy())? {
            Some(text) => Policy::parse(&text)?,
            None => Policy::default(),
        };
        let scope_text = read_optional(&paths.scope())?;
        let scope = match &scope_text {
            Some(text) => Scope::parse(text, &policy)?,
            None => Scope::default(),
        };
        Ok(Self {
            scope: Arc::new(scope),
            policy: Arc::new(policy),
            scope_missing: scope_text.is_none(),
        })
    }
}

fn read_optional(path: &std::path::Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Reconnect counters the status JSON reports. In memory for the life of the process.
pub struct Health {
    /// `true` while the update stream is down and a reconnect is in progress.
    pub reconnecting: AtomicBool,
    /// Unix times of reconnects that succeeded.
    pub reconnect_at: Mutex<Vec<i64>>,
    /// Unix time of the latest drop. `0` when none has happened.
    pub last_drop_at: AtomicI64,
    /// Seconds the latest drop lasted. `-1` when none has finished.
    pub last_drop_secs: AtomicI64,
    /// `true` when the latest catch-up did not close the gap.
    pub gap_unfilled: AtomicBool,
    /// `true` after enough non-link read failures, or while a flood wait is in progress.
    pub degraded: AtomicBool,
    degraded_reason: Mutex<Option<String>>,
    flood_until: Mutex<Option<Instant>>,
}

impl Health {
    pub(crate) fn new() -> Self {
        Self {
            reconnecting: AtomicBool::new(false),
            reconnect_at: Mutex::new(Vec::new()),
            last_drop_at: AtomicI64::new(0),
            last_drop_secs: AtomicI64::new(-1),
            gap_unfilled: AtomicBool::new(false),
            degraded: AtomicBool::new(false),
            degraded_reason: Mutex::new(None),
            flood_until: Mutex::new(None),
        }
    }

    fn reason_slot(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        match self.degraded_reason.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn flood_slot(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        match self.flood_until.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Records the class of non-link read failures inside the failure window.
    pub(crate) fn mark_degraded(&self, reason: &str) {
        self.degraded.store(true, Ordering::Relaxed);
        *self.reason_slot() = Some(reason.to_owned());
        *self.flood_slot() = None;
    }

    /// Marks an in-progress flood. `connection` is `degraded` and the reason is `flood_wait`.
    pub(crate) fn mark_flood(&self, remaining: Duration) {
        self.degraded.store(true, Ordering::Relaxed);
        *self.reason_slot() = Some("flood_wait".to_owned());
        *self.flood_slot() = Some(Instant::now() + remaining);
    }

    /// Seconds left on the in-progress flood, or `None` when there is not one.
    pub(crate) fn flood_wait_secs(&self) -> Option<u64> {
        let until = (*self.flood_slot())?;
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            None
        } else {
            // A partial second still counts, so a 31 s wait does not show 30 immediately.
            Some(left.as_secs() + u64::from(left.subsec_nanos() != 0))
        }
    }

    /// Clears a degraded read. Called when an update arrives or the stream is restored.
    pub(crate) fn clear_degraded(&self) {
        self.degraded.store(false, Ordering::Relaxed);
        *self.reason_slot() = None;
        *self.flood_slot() = None;
    }

    pub(crate) fn degraded_reason(&self) -> Option<String> {
        self.reason_slot().clone()
    }

    /// Status `connection` value. Unauthorized wins, then reconnecting, then degraded.
    pub(crate) fn connection(&self, unauthorized: bool) -> &'static str {
        if unauthorized {
            "unauthorized"
        } else if self.reconnecting.load(Ordering::Relaxed) {
            "reconnecting"
        } else if self.degraded.load(Ordering::Relaxed) {
            "degraded"
        } else {
            "connected"
        }
    }

    pub(crate) fn begin_drop(&self, at: i64) {
        self.reconnecting.store(true, Ordering::Relaxed);
        self.gap_unfilled.store(true, Ordering::Relaxed);
        self.last_drop_at.store(at, Ordering::Relaxed);
        self.last_drop_secs.store(-1, Ordering::Relaxed);
    }

    pub(crate) fn end_drop(&self, at: i64, secs: i64, gap_closed: bool) {
        self.reconnecting.store(false, Ordering::Relaxed);
        self.gap_unfilled.store(!gap_closed, Ordering::Relaxed);
        self.last_drop_secs.store(secs, Ordering::Relaxed);
        let mut times = match self.reconnect_at.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        times.push(at);
        let cutoff = at - SECS_PER_DAY;
        times.retain(|stamp| *stamp >= cutoff);
    }

    pub(crate) fn reconnects_24h(&self, now: i64) -> usize {
        let times = match self.reconnect_at.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let cutoff = now - SECS_PER_DAY;
        times.iter().filter(|stamp| **stamp >= cutoff).count()
    }
}

/// What only a running daemon has: the Telegram handle and the pipeline.
pub struct Live {
    pub handle: Handle,
    pub me: ChatSummary,
    pub events: broadcast::Sender<InboundEvent>,
    pub pipeline: mpsc::UnboundedSender<PipelineInput>,
    pub last_update_at: AtomicI64,
    pub open_bursts: AtomicUsize,
    pub caught_up: AtomicU64,
    pub unauthorized: AtomicBool,
    pub health: Health,
}

/// Everything the daemon's tasks share.
pub struct App {
    pub db: Db,
    pub paths: Paths,
    pub profile: String,
    pub config: RwLock<Config>,
    pub paused: AtomicBool,
    pub started_at: i64,
    pub live: Option<Live>,
    pub ui_url: RwLock<Option<String>>,
}

impl App {
    pub fn config(&self) -> Config {
        self.config
            .read()
            .map(|c| c.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    fn live(&self) -> Result<&Live> {
        self.live
            .as_ref()
            .ok_or_else(|| anyhow!("the daemon is not running; this needs a Telegram connection"))
    }

    pub async fn status(&self) -> Result<Value> {
        let since = now_unix() - SECS_PER_DAY;
        let counts = self.db.run(move |s| s.ticket_counts(since)).await?;
        let counts: serde_json::Map<String, Value> = TicketState::ALL
            .iter()
            .map(|state| {
                let n = counts
                    .iter()
                    .find(|(s, _)| s == state)
                    .map_or(0, |(_, n)| *n);
                (state.as_str().to_owned(), json!(n))
            })
            .collect();
        let config = self.config();
        let (connection, me, last_update_at, open_bursts, health) = match &self.live {
            Some(live) => {
                let unauthorized = live.unauthorized.load(Ordering::Relaxed);
                (
                    live.health.connection(unauthorized),
                    Some(&live.me),
                    if unauthorized {
                        None
                    } else {
                        Some(live.last_update_at.load(Ordering::Relaxed)).filter(|t| *t > 0)
                    },
                    if unauthorized {
                        0
                    } else {
                        live.open_bursts.load(Ordering::Relaxed)
                    },
                    Some(&live.health),
                )
            }
            None => ("offline", None, None, 0, None),
        };
        let now = now_unix();
        let last_drop_at = health
            .map(|h| h.last_drop_at.load(Ordering::Relaxed))
            .filter(|t| *t > 0);
        let last_drop_secs = health
            .map(|h| h.last_drop_secs.load(Ordering::Relaxed))
            .filter(|secs| *secs >= 0);
        Ok(json!({
            "profile": self.profile,
            "connection": connection,
            "degraded_reason": health.and_then(|h| h.degraded_reason()),
            "flood_wait_secs": health.and_then(|h| h.flood_wait_secs()),
            "reconnects_24h": health.map(|h| h.reconnects_24h(now)).unwrap_or(0),
            "last_drop_at": last_drop_at,
            "last_drop_secs": last_drop_secs,
            "gap_unfilled": health.is_some_and(|h| h.gap_unfilled.load(Ordering::Relaxed)),
            "me": me,
            "paused": self.paused.load(Ordering::Relaxed),
            "started_at": self.started_at,
            "now": now_unix(),
            "last_update_at": last_update_at,
            "open_bursts": open_bursts,
            "tickets_last_day": counts,
            "scope_chats": config.scope.entries().len(),
            "scope_missing": config.scope_missing,
            "ui_url": self.ui_url.read().ok().and_then(|u| u.clone()),
        }))
    }

    pub async fn set_paused(&self, paused: bool) -> Result<Value> {
        self.db.run(move |s| s.set_paused(paused)).await?;
        self.paused.store(paused, Ordering::Relaxed);
        tracing::info!(paused, "pipeline pause changed");
        Ok(json!({ "paused": paused }))
    }

    pub async fn tickets(
        &self,
        state: Option<&str>,
        trigger: Option<&str>,
        within_secs: Option<i64>,
        unlabelled: bool,
        limit: Option<usize>,
    ) -> Result<Value> {
        let filter = TicketFilter {
            state: state
                .map(str::parse::<TicketState>)
                .transpose()
                .map_err(|e| anyhow!(e))?,
            trigger: trigger
                .map(str::parse::<TriggerKind>)
                .transpose()
                .map_err(|e| anyhow!(e))?,
            since: within_secs.map(|w| now_unix() - w),
            unlabelled,
            limit,
        };
        let rows = self.db.run(move |s| s.tickets(&filter)).await?;
        Ok(json!(rows))
    }

    /// One ticket. Message text is only included when `with_text` is set.
    pub async fn ticket(&self, id: i64, with_text: bool) -> Result<Value> {
        let Some(mut detail) = self.db.run(move |s| s.ticket(id)).await? else {
            bail!("ticket {id} does not exist");
        };
        if !with_text {
            for body in &mut detail.bodies {
                body.text = None;
            }
        }
        Ok(json!(detail))
    }

    pub async fn label(&self, id: i64, label: Option<&str>) -> Result<Value> {
        let label = label
            .map(str::parse::<ExplicitLabel>)
            .transpose()
            .map_err(|e| anyhow!(e))?;
        let now = now_unix();
        if !self.db.run(move |s| s.label_ticket(id, label, now)).await? {
            bail!("ticket {id} does not exist");
        }
        Ok(json!({ "id": id, "label": label }))
    }

    pub async fn report(&self, within_secs: i64) -> Result<Value> {
        let until = now_unix();
        let since = until - within_secs.max(1);
        let mut input = self.db.run(move |s| s.report_input(since, until)).await?;
        if self.live.is_some() {
            // The store only knows this run up to its last heartbeat, but it is still running.
            for run in &mut input.runs {
                if run.started_at == self.started_at {
                    run.stopped_at = until;
                }
            }
        }
        let policy = self.config().policy;
        Ok(json!(report::build(&input, &policy)))
    }

    pub fn scope_view(&self) -> Value {
        let config = self.config();
        json!({
            "missing": config.scope_missing,
            "path": self.paths.scope(),
            "entries": config.scope.entries(),
            "policy": *config.policy,
        })
    }

    pub fn reload(&self) -> Result<Value> {
        let config = Config::load(&self.paths)?;
        let chats = config.scope.entries().len();
        match self.config.write() {
            Ok(mut current) => *current = config,
            Err(poisoned) => *poisoned.into_inner() = config,
        }
        tracing::info!(chats, "scope reloaded");
        Ok(json!({ "scope_chats": chats }))
    }

    pub async fn chats(&self, limit: usize) -> Result<Value> {
        let dialogs = self.live()?.handle.dialogs(limit).await?;
        Ok(json!(dialogs))
    }

    pub async fn contacts(&self) -> Result<Value> {
        let contacts = self.live()?.handle.contacts().await?;
        Ok(json!(contacts))
    }

    pub async fn upsert_contact(&self, spec: captain_core::ContactUpsert) -> Result<Value> {
        self.live()?.handle.upsert_contact(spec).await?;
        Ok(json!({ "updated": true }))
    }

    pub async fn delete_user_history(
        &self,
        chat: captain_core::ChatRef,
        just_clear: bool,
    ) -> Result<Value> {
        let deleted = self
            .live()?
            .handle
            .delete_user_history(&chat, just_clear)
            .await?;
        Ok(json!(deleted))
    }

    pub async fn folders(&self) -> Result<Value> {
        let folders = self.live()?.handle.dialog_folders().await?;
        Ok(json!(folders))
    }

    pub async fn upsert_folder(&self, spec: captain_core::DialogFolderSpec) -> Result<Value> {
        self.live()?.handle.upsert_dialog_folder(spec).await?;
        Ok(json!({ "updated": true }))
    }

    pub async fn delete_folder(&self, id: i32) -> Result<Value> {
        self.live()?.handle.delete_dialog_folder(id).await?;
        Ok(json!({ "deleted": id }))
    }

    pub async fn reorder_folders(&self, order: Vec<i32>) -> Result<Value> {
        self.live()?.handle.reorder_dialog_folders(order).await?;
        Ok(json!({ "reordered": true }))
    }

    pub async fn create_group(&self, title: String, about: String) -> Result<Value> {
        let created = self
            .live()?
            .handle
            .create_private_group(title, about)
            .await?;
        Ok(json!(created))
    }

    pub async fn run_ticket(&self, id: i64) -> Result<Value> {
        crate::runner::run_ticket_now(self, id).await
    }

    pub async fn runs(&self, limit: usize) -> Result<Value> {
        let rows = crate::runner::recent_runs(self, limit).await?;
        Ok(json!(rows))
    }

    /// Reads up to `scan_max` messages of `chat` older than `before` from Telegram and returns
    /// those whose text matches `pattern`, oldest first. Nothing is stored or marked read.
    pub async fn history(
        &self,
        chat: captain_core::ChatId,
        pattern: &str,
        before: Option<captain_core::MessageId>,
        scan_max: usize,
    ) -> Result<Value> {
        let regex = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("invalid pattern {pattern:?}"))?;
        let scan_max = scan_max.clamp(1, HISTORY_SCAN_CAP);
        let messages = self.live()?.handle.history(chat, before, scan_max).await?;
        let scanned = messages.len();
        let matches = history_matches(chat, &messages, &regex);
        tracing::info!(
            chat = chat.0,
            scanned,
            matches = matches.len(),
            "history searched"
        );
        Ok(json!({
            "chat": chat,
            "scanned": scanned,
            "oldest_scanned": messages.last().map(|m| m.id),
            "more_before": scanned == scan_max,
            "matches": matches,
        }))
    }

    pub async fn send(&self, message: captain_core::OutgoingMessage) -> Result<Value> {
        let sent = self.live()?.handle.send(message).await?;
        Ok(json!({ "sent": sent }))
    }

    pub async fn download(
        &self,
        chat: captain_core::ChatId,
        message: captain_core::MessageId,
        out: std::path::PathBuf,
    ) -> Result<Value> {
        let bytes = self
            .live()?
            .handle
            .download_media(chat, message)
            .await?
            .ok_or_else(|| anyhow!("message {} in chat {} has no media", message.0, chat.0))?;
        std::fs::write(&out, &bytes).with_context(|| format!("cannot write {}", out.display()))?;
        Ok(json!({ "path": out, "size_bytes": bytes.len() }))
    }

    pub async fn new_ticket(
        &self,
        chat: captain_core::ChatId,
        message: captain_core::MessageId,
        note: Option<String>,
    ) -> Result<Value> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.live()?
            .pipeline
            .send(PipelineInput::Manual {
                pointers: vec![captain_inbox::manual::Pointer { chat, message }],
                note,
                reply: Some(reply),
            })
            .map_err(|_| anyhow!("the pipeline has stopped"))?;
        let id = answer
            .await
            .map_err(|_| anyhow!("the pipeline has stopped"))??;
        Ok(json!({ "ticket": id }))
    }
}

/// The messages, given newest first, whose text matches `regex`, oldest first.
fn history_matches(
    chat: captain_core::ChatId,
    messages: &[captain_core::MessageView],
    regex: &regex::Regex,
) -> Vec<Value> {
    messages
        .iter()
        .rev()
        .filter(|m| regex.is_match(&m.text.text))
        .map(|m| {
            json!({
                "id": m.id,
                "date_unix": m.date_unix,
                "sender": m.sender.as_ref().and_then(|s| s.name.clone()),
                "text": m.text.text,
                "link": chat
                    .channel_id()
                    .map(|c| format!("{PRIVATE_POST_URL}/{c}/{}", m.id.0)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use captain_core::{ChatId, ChatKind, FormattedText, MessageId, MessageView};

    fn message(chat: i64, id: i32, text: &str) -> MessageView {
        let summary = |id: i64, kind| ChatSummary {
            id: ChatId(id),
            kind,
            name: Some(format!("chat {id}")),
            username: None,
            is_bot: false,
        };
        MessageView {
            chat: summary(chat, ChatKind::Channel),
            id: MessageId(id),
            sender: Some(summary(7, ChatKind::User)),
            outgoing: false,
            date_unix: i64::from(id),
            text: FormattedText::plain(text),
            media: None,
            reply_to: None,
            album_id: None,
            mentioned: false,
            topic: None,
            quote_text: None,
            forward: None,
            edited_at_unix: None,
        }
    }

    #[test]
    fn history_matches_ignore_case_and_come_oldest_first() {
        let chat = -1_001_234_567_890;
        let newest_first = [
            message(chat, 3, "GIỚI THIỆU lại tính năng"),
            message(chat, 2, "không liên quan"),
            message(chat, 1, "Mình giới thiệu tính năng nhé"),
        ];
        let regex = regex::RegexBuilder::new("giới thiệu.*tính năng")
            .case_insensitive(true)
            .build()
            .unwrap();
        let found = history_matches(ChatId(chat), &newest_first, &regex);
        let ids: Vec<i64> = found.iter().map(|m| m["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, [1, 3]);
        assert_eq!(found[0]["link"], "https://t.me/c/1234567890/1");

        let group = history_matches(ChatId(-5_000_000_001), &newest_first[2..], &regex);
        assert!(group[0]["link"].is_null());
    }

    #[test]
    fn reconnects_count_for_one_day() {
        use std::sync::atomic::Ordering;

        let health = Health::new();
        let at = 1_700_000_000;
        health.begin_drop(at);
        assert!(health.reconnecting.load(Ordering::Relaxed));
        assert!(health.gap_unfilled.load(Ordering::Relaxed));
        assert_eq!(health.last_drop_secs.load(Ordering::Relaxed), -1);
        assert_eq!(health.connection(false), "reconnecting");
        health.mark_degraded("other");
        assert_eq!(health.connection(false), "reconnecting");
        assert_eq!(health.connection(true), "unauthorized");
        health.end_drop(at, 4, true);
        assert!(!health.reconnecting.load(Ordering::Relaxed));
        assert!(!health.gap_unfilled.load(Ordering::Relaxed));
        assert_eq!(health.last_drop_at.load(Ordering::Relaxed), at);
        assert_eq!(health.last_drop_secs.load(Ordering::Relaxed), 4);
        assert_eq!(health.reconnects_24h(at), 1);
        assert_eq!(health.reconnects_24h(at + super::SECS_PER_DAY), 1);
        assert_eq!(health.reconnects_24h(at + super::SECS_PER_DAY + 1), 0);
        assert_eq!(health.connection(false), "degraded");
        assert_eq!(health.degraded_reason().as_deref(), Some("other"));
        health.clear_degraded();
        assert_eq!(health.connection(false), "connected");
        assert!(health.degraded_reason().is_none());
        health.mark_flood(Duration::from_secs(31));
        assert_eq!(health.connection(false), "degraded");
        assert_eq!(health.degraded_reason().as_deref(), Some("flood_wait"));
        assert_eq!(health.flood_wait_secs(), Some(31));
        health.clear_degraded();
        assert!(health.flood_wait_secs().is_none());
    }
}
