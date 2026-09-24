//! State shared by the daemon's tasks, and the operations both the socket and the web UI offer.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use captain_core::{ChatSummary, InboundEvent};
use captain_inbox::{ExplicitLabel, Policy, Scope, TicketState, TriggerKind, report};
use captain_protocol::Handle;
use captain_store::{Store, TicketFilter};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::paths::Paths;
use crate::pipeline::PipelineInput;

const SECS_PER_DAY: i64 = 86_400;

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
        let (connection, me, last_update_at, open_bursts) = match &self.live {
            Some(live) if live.unauthorized.load(Ordering::Relaxed) => {
                ("unauthorized", Some(&live.me), None, 0)
            }
            Some(live) => (
                "connected",
                Some(&live.me),
                Some(live.last_update_at.load(Ordering::Relaxed)).filter(|t| *t > 0),
                live.open_bursts.load(Ordering::Relaxed),
            ),
            None => ("offline", None, None, 0),
        };
        Ok(json!({
            "profile": self.profile,
            "connection": connection,
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
