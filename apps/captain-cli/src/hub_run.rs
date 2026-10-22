//! The bridge process: sidecar, long poll, bridge, health, and shutdown.
//!
//! This task holds `hub.lock`. The Captain daemon holds `session.lock`. They share the
//! `SQLite` file and nothing else.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use captain_vault::Secret;
use hub_bridge::{
    Bridge, BridgeConfig, BridgeStore, SqliteStore, SystemClock, TopicIncoming as BridgeIncoming,
};
use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, ModuleHealth, ModuleState,
    NetworkAdapter, OutgoingMessage, RemoteEvent, TgMessageId, TopicId, TopicSink,
};
use hub_store::{LogLevel, NewLogEvent, SharedHub};
use hub_tgbot::{BotClient, BotConfig, BotToken, IncomingMessage, OffsetStore, TopicIncoming};
use hub_web::limits::{MODULE_BRIDGE, MODULE_ZALO_BRIDGE};
use hub_zalo::{Sidecar, SidecarConfig, SidecarStatus, ZaloAdapter, ZaloAdmin};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::hub::{decode_base64, render_qr_blocks, write_qr_png};
use crate::hub_config::HubConfig;
use crate::hub_map::{MappedUpdate, map_update, merge_album, prepare_outgoing};

const LOCK_FILE: &str = "hub.lock";
const STORE_FILE: &str = "inbox.sqlite"; // same file as `Paths::store`
const ZALO_DIR: &str = "zalo";
const CREDENTIALS_FILE: &str = "credentials.json";
const OFFSET_KEY: &str = "hub_bot_update_offset";
const DEFAULT_API_BASE: &str = "https://api.telegram.org";
const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const STATE_POLL: Duration = Duration::from_secs(1);
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const ALBUM_WAIT: Duration = Duration::from_millis(400);
const UPDATE_CHANNEL: usize = 64;
const TOPIC_LIST_CAP: usize = 30;
const KIND_LISTENER_DROP: &str = "listener_drop";
const KIND_RECONNECT: &str = "reconnect";
const KIND_QR_RELOGIN: &str = "qr_relogin";
const KIND_RATE_LIMIT: &str = "rate_limit";
const KIND_FAILED_EVENT: &str = "failed_event";
const CMD_STATUS: &str = "status";
const CMD_LOGIN: &str = "login";
const CMD_CHECK: &str = "checktopics";
const CMD_TOPIC: &str = "topic";
const CMD_RECALL: &str = "recall";
const CMD_LIST: &str = "list";
const CMD_INFO: &str = "info";

/// What `captain hub run` starts. Tests pass a fake API base and a short restart delay.
pub struct RunOptions {
    /// Profile state directory. `hub.toml` and `inbox.sqlite` live here.
    pub state_dir: PathBuf,
    /// Bot token. Not read from the environment.
    pub token: Secret,
    /// Bot API origin, with no token in it.
    pub api_base: String,
    /// Sidecar restart delay. `None` keeps the sidecar default.
    pub restart_initial: Option<Duration>,
}

/// A running bridge. Drop asks the task to stop and aborts it if it is still running.
#[cfg_attr(not(test), allow(dead_code))]
pub struct RunningHub {
    stop: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<Result<()>>>>,
    sidecar: Arc<Sidecar>,
    hub: SharedHub,
    _lock: HubLock,
}

/// Held until this process drops it. The kernel releases it if the process dies.
#[derive(Debug)]
pub struct HubLock {
    _file: File,
}

/// One text command posted in the bridge group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slash {
    /// Command without the slash or `@bot` suffix.
    pub name: String,
    /// First argument, lowercased. Empty when there is none.
    pub arg: String,
}

/// Parses a leading slash command. Other text returns `None`.
pub fn slash_command(text: &str) -> Option<Slash> {
    let rest = text.trim().strip_prefix('/')?;
    let mut parts = rest.split_whitespace();
    let head = parts.next()?;
    let name = head.split('@').next()?.to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let arg = parts.next().unwrap_or("").to_ascii_lowercase();
    Some(Slash { name, arg })
}

/// Takes `hub.lock` in `dir` without waiting.
///
/// # Errors
///
/// Returns an error when another hub process already holds the lock.
pub fn lock_hub(dir: &Path) -> Result<HubLock> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let file = File::create(&path).with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(HubLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("another hub process owns this state directory")
        }
        Err(std::fs::TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

impl RunningHub {
    /// Sidecar supervisor, for tests that emit fixture events.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn sidecar(&self) -> &Arc<Sidecar> {
        &self.sidecar
    }

    /// Shared store this process writes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn hub(&self) -> &SharedHub {
        &self.hub
    }

    /// Stops readers, drains the bridge queue, and shuts the sidecar down.
    ///
    /// # Errors
    ///
    /// Returns an error when the task failed. A panic payload is not included.
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.stop.send(true);
        let task = self
            .task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        let result = match task {
            Some(task) => match task.await {
                Ok(result) => result,
                Err(_) => bail!("hub task stopped"),
            },
            None => Ok(()),
        };
        drop(self);
        result
    }
}

impl Drop for RunningHub {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        let slot = self.task.get_mut().unwrap_or_else(|err| err.into_inner());
        if let Some(task) = slot.take() {
            task.abort();
        }
    }
}

/// Starts the sidecar, the bridge, and the Telegram long poll.
///
/// # Errors
///
/// Returns an error when the lock, the config, the store, or the sidecar fails.
/// The bot token is not copied into that error.
pub async fn start_hub(options: RunOptions) -> Result<RunningHub> {
    let lock = lock_hub(&options.state_dir)?;
    let config = HubConfig::load(&options.state_dir)?;
    let hub = SharedHub::open(&options.state_dir.join(STORE_FILE))
        .with_context(|| "cannot open the hub store".to_owned())?;
    {
        let hub = hub.clone();
        let group_id = config.group_id;
        tokio::task::spawn_blocking(move || hub.lock().set_bridge_supergroup(group_id))
            .await
            .context("store task stopped")?
            .context("cannot store the bridge group id")?;
    }
    let data_dir = options.state_dir.join(ZALO_DIR);
    let credentials = data_dir.join(CREDENTIALS_FILE);
    ensure_credentials(&credentials)?;
    let mut sidecar_config = SidecarConfig::new(
        OsString::from(&config.sidecar_program),
        config.sidecar_args.iter().map(OsString::from).collect(),
        credentials,
        data_dir.clone(),
    );
    if let Some(initial) = options.restart_initial {
        sidecar_config.restart_initial = initial;
    }
    let (adapter, admin) = ZaloAdapter::start(sidecar_config)
        .map_err(|_| anyhow::anyhow!("cannot start the zalo sidecar"))?;
    let sidecar = Arc::clone(adapter.sidecar());
    let store = SqliteStore::new(hub.clone());
    let gate = Gate {
        inner: adapter,
        hub: hub.clone(),
        skip_muted: config.skip_muted_zalo_groups,
    };
    let bot = BotClient::new(BotConfig {
        token: BotToken::new(options.token.expose().to_owned()),
        api_base_url: options.api_base,
        group_id: config.group_id,
    })
    .map_err(|_| anyhow::anyhow!("cannot build the bot client"))?;
    let bridge = Bridge::new(
        BridgeConfig {
            ops_topic: config.ops_topic_id.map(TopicId),
        },
        vec![Box::new(gate)],
        bot.clone(),
        store,
        SystemClock,
    );
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(supervise(
        stop_rx,
        hub.clone(),
        sidecar.clone(),
        bridge,
        bot,
        admin,
        config,
        data_dir,
        options.state_dir.clone(),
    ));
    Ok(RunningHub {
        stop,
        task: Mutex::new(Some(task)),
        sidecar,
        hub,
        _lock: lock,
    })
}

/// Default Bot API origin.
pub fn default_api_base() -> &'static str {
    DEFAULT_API_BASE
}

fn ensure_credentials(path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    captain_vault::write_private(path, b"{}")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    mut stop: watch::Receiver<bool>,
    hub: SharedHub,
    sidecar: Arc<Sidecar>,
    bridge: Bridge<BotClient, SqliteStore, SystemClock>,
    bot: BotClient,
    admin: ZaloAdmin,
    config: HubConfig,
    data_dir: PathBuf,
    state_dir: PathBuf,
) -> Result<()> {
    let (bridge_stop_tx, mut bridge_stop_rx) = watch::channel(false);
    let bridge_task = {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            bridge
                .run(async move {
                    let _ = bridge_stop_rx.changed().await;
                })
                .await
        })
    };
    let (tx, rx) = mpsc::channel(UPDATE_CHANNEL);
    let offsets = SettingsOffset { hub: hub.clone() };
    let poll_stop = stop.clone();
    let poller = bot.clone();
    let poll_task = tokio::spawn(async move { poller.run_updates(&offsets, tx, poll_stop).await });
    let admin = Arc::new(admin);
    let dispatch_task = tokio::spawn(dispatch(
        stop.clone(),
        rx,
        bridge.clone(),
        bot,
        admin,
        hub.clone(),
        config.clone(),
        state_dir,
    ));
    let health_task = tokio::spawn(health_loop(
        stop.clone(),
        hub.clone(),
        sidecar.clone(),
        bridge,
        config,
        data_dir,
    ));
    let _ = stop.changed().await;
    let _ = poll_task.await;
    let _ = dispatch_task.await;
    let _ = bridge_stop_tx.send(true);
    let bridge_result = bridge_task.await;
    let _ = health_task.await;
    sidecar.shutdown().await;
    write_health(
        &hub,
        ModuleHealth {
            module: MODULE_ZALO_BRIDGE.to_owned(),
            state: ModuleState::Down,
            detail: "stopped".into(),
            updated_at_unix_secs: unix_now(),
        },
    )
    .await;
    write_health(
        &hub,
        ModuleHealth {
            module: MODULE_BRIDGE.to_owned(),
            state: ModuleState::Down,
            detail: "stopped".into(),
            updated_at_unix_secs: unix_now(),
        },
    )
    .await;
    match bridge_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => bail!("bridge stopped"),
        Err(_) => bail!("hub task stopped"),
    }
}

async fn health_loop(
    mut stop: watch::Receiver<bool>,
    hub: SharedHub,
    sidecar: Arc<Sidecar>,
    bridge: Bridge<BotClient, SqliteStore, SystemClock>,
    config: HubConfig,
    data_dir: PathBuf,
) {
    let mut events = sidecar.subscribe();
    let mut phase = sidecar.watch_phase();
    let mut last = sidecar.status();
    let mut last_bridge = bridge.health();
    let mut failures = failure_count(&hub).await;
    write_health(&hub, sidecar.health()).await;
    write_health(&hub, last_bridge.clone()).await;
    let mut health_tick = tokio::time::interval(HEALTH_INTERVAL);
    let mut state_tick = tokio::time::interval(STATE_POLL);
    let mut prune_tick = tokio::time::interval(PRUNE_INTERVAL);
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    state_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    health_tick.tick().await;
    state_tick.tick().await;
    prune_tick.tick().await;
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = health_tick.tick() => {
                write_health(&hub, sidecar.health()).await;
                let snapshot = bridge.health();
                note_bridge(&hub, &mut last_bridge, &snapshot, &mut failures).await;
                write_health(&hub, snapshot).await;
            }
            _ = state_tick.tick() => {
                note_sidecar(&hub, &sidecar, &mut last).await;
                let snapshot = bridge.health();
                note_bridge(&hub, &mut last_bridge, &snapshot, &mut failures).await;
            }
            _ = phase.changed() => {
                note_sidecar(&hub, &sidecar, &mut last).await;
            }
            event = events.recv() => {
                if event.is_err() && matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    break;
                }
                note_sidecar(&hub, &sidecar, &mut last).await;
            }
            _ = prune_tick.tick() => {
                prune(&hub, &data_dir, config.event_retention_days, config.media_retention_days).await;
            }
        }
    }
}

async fn note_sidecar(hub: &SharedHub, sidecar: &Sidecar, last: &mut SidecarStatus) {
    let now = sidecar.status();
    if last.listener_up && !now.listener_up {
        write_event(
            hub,
            MODULE_ZALO_BRIDGE,
            LogLevel::Warn,
            KIND_LISTENER_DROP,
            "listener down",
        )
        .await;
    }
    if now.spawn_count > last.spawn_count && last.spawn_count > 0 {
        write_event(
            hub,
            MODULE_ZALO_BRIDGE,
            LogLevel::Info,
            KIND_RECONNECT,
            "sidecar restarted",
        )
        .await;
    }
    let changed = now.phase != last.phase
        || now.listener_up != last.listener_up
        || now.session != last.session
        || now.spawn_count != last.spawn_count;
    if changed {
        write_health(hub, sidecar.health()).await;
    }
    *last = now;
}

async fn note_bridge(
    hub: &SharedHub,
    last: &mut ModuleHealth,
    snapshot: &ModuleHealth,
    failures: &mut usize,
) {
    let limited = snapshot.detail.contains("rate_limited=true");
    let was_limited = last.detail.contains("rate_limited=true");
    if limited && !was_limited {
        write_event(
            hub,
            MODULE_BRIDGE,
            LogLevel::Warn,
            KIND_RATE_LIMIT,
            "telegram 429",
        )
        .await;
    }
    let count = failure_count(hub).await;
    if count > *failures {
        write_event(
            hub,
            MODULE_BRIDGE,
            LogLevel::Error,
            KIND_FAILED_EVENT,
            "delivery failed",
        )
        .await;
    }
    *failures = count;
    if snapshot.state != last.state || snapshot.detail != last.detail {
        write_health(hub, snapshot.clone()).await;
    }
    *last = snapshot.clone();
}

async fn failure_count(hub: &SharedHub) -> usize {
    let hub = hub.clone();
    tokio::task::spawn_blocking(move || hub.lock().reader().failures(true).map(|rows| rows.len()))
        .await
        .ok()
        .and_then(|result| result.ok())
        .unwrap_or(0)
}

async fn prune(hub: &SharedHub, data_dir: &Path, event_days: i64, media_days: i64) {
    let hub = hub.clone();
    let now = unix_now();
    let _ = tokio::task::spawn_blocking(move || hub.lock().prune_events(now, event_days)).await;
    let media = data_dir.join("media");
    let retention = Duration::from_secs(
        u64::try_from(media_days)
            .unwrap_or(0)
            .saturating_mul(86_400),
    );
    let _ = hub_zalo::cleanup_expired_media(&media, SystemTime::now(), retention);
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    mut stop: watch::Receiver<bool>,
    mut rx: mpsc::Receiver<TopicIncoming>,
    bridge: Bridge<BotClient, SqliteStore, SystemClock>,
    bot: BotClient,
    admin: Arc<ZaloAdmin>,
    hub: SharedHub,
    config: HubConfig,
    state_dir: PathBuf,
) {
    let store = SqliteStore::new(hub.clone());
    let mut pending: Option<AlbumHold> = None;
    let mut extra: Vec<JoinHandle<()>> = Vec::new();
    loop {
        if *stop.borrow() {
            flush_album(&mut pending, &bridge).await;
            while let Ok(update) = rx.try_recv() {
                handle_update(
                    update,
                    &mut pending,
                    &bridge,
                    &bot,
                    &admin,
                    &hub,
                    &store,
                    &config,
                    &state_dir,
                    &mut extra,
                )
                .await;
            }
            break;
        }
        let deadline = pending.as_ref().map(|hold| hold.deadline);
        let album_wait = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(album_wait);
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    continue;
                }
            }
            update = rx.recv() => {
                match update {
                    Some(update) => {
                        handle_update(
                            update,
                            &mut pending,
                            &bridge,
                            &bot,
                            &admin,
                            &hub,
                            &store,
                            &config,
                            &state_dir,
                            &mut extra,
                        )
                        .await;
                    }
                    None => break,
                }
            }
            _ = &mut album_wait => {
                flush_album(&mut pending, &bridge).await;
            }
        }
    }
    for task in extra {
        task.abort();
    }
}

struct AlbumHold {
    id: String,
    parts: Vec<IncomingMessage>,
    deadline: tokio::time::Instant,
}

#[allow(clippy::too_many_arguments)]
async fn handle_update(
    update: TopicIncoming,
    pending: &mut Option<AlbumHold>,
    bridge: &Bridge<BotClient, SqliteStore, SystemClock>,
    bot: &BotClient,
    admin: &Arc<ZaloAdmin>,
    hub: &SharedHub,
    store: &SqliteStore,
    config: &HubConfig,
    state_dir: &Path,
    extra: &mut Vec<JoinHandle<()>>,
) {
    let TopicIncoming::Message(message) = &update else {
        flush_album(pending, bridge).await;
        deliver_mapped(map_update(update), bridge, store, hub).await;
        return;
    };
    if message.attachments.is_empty()
        && let Some(slash) = slash_command(&message.text)
    {
        flush_album(pending, bridge).await;
        run_command(
            slash, message, bot, admin, hub, store, bridge, config, state_dir, extra,
        )
        .await;
        return;
    }
    if let Some(group) = message.media_group_id.clone() {
        let same = pending.as_ref().is_some_and(|hold| hold.id == group);
        if !same {
            flush_album(pending, bridge).await;
        }
        let hold = pending.get_or_insert_with(|| AlbumHold {
            id: group,
            parts: Vec::new(),
            deadline: tokio::time::Instant::now() + ALBUM_WAIT,
        });
        hold.deadline = tokio::time::Instant::now() + ALBUM_WAIT;
        hold.parts.push(message.clone());
        return;
    }
    flush_album(pending, bridge).await;
    deliver_mapped(map_update(update), bridge, store, hub).await;
}

async fn flush_album(
    pending: &mut Option<AlbumHold>,
    bridge: &Bridge<BotClient, SqliteStore, SystemClock>,
) {
    let Some(hold) = pending.take() else {
        return;
    };
    let incoming = prepare_outgoing(merge_album(hold.parts));
    let _ = deliver(bridge, incoming).await;
}

async fn deliver_mapped(
    mapped: MappedUpdate,
    bridge: &Bridge<BotClient, SqliteStore, SystemClock>,
    store: &SqliteStore,
    hub: &SharedHub,
) {
    match mapped {
        MappedUpdate::Message(incoming) => {
            let _ = deliver(bridge, prepare_outgoing(incoming)).await;
        }
        MappedUpdate::Reaction {
            topic,
            tg_message,
            emoji,
            added,
        } => {
            let Some(topic) = resolve_topic(topic, tg_message, store).await else {
                write_event(
                    hub,
                    MODULE_BRIDGE,
                    LogLevel::Warn,
                    KIND_FAILED_EVENT,
                    "reaction has no topic",
                )
                .await;
                return;
            };
            let _ = deliver(
                bridge,
                BridgeIncoming::Reaction {
                    topic,
                    tg_message,
                    emoji,
                    added,
                },
            )
            .await;
        }
    }
}

async fn resolve_topic(
    topic: Option<TopicId>,
    tg_message: TgMessageId,
    store: &SqliteStore,
) -> Option<TopicId> {
    if topic.is_some() {
        return topic;
    }
    let reference = store.message_for_telegram(tg_message).await.ok()??;
    let mapping = store.topic_for(&reference.conversation).await.ok()??;
    Some(mapping.topic)
}

async fn deliver(
    bridge: &Bridge<BotClient, SqliteStore, SystemClock>,
    incoming: BridgeIncoming,
) -> Result<()> {
    bridge
        .handle_topic_message(incoming)
        .await
        .map_err(|_| anyhow::anyhow!("delivery failed"))
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    slash: Slash,
    message: &IncomingMessage,
    bot: &BotClient,
    admin: &Arc<ZaloAdmin>,
    hub: &SharedHub,
    store: &SqliteStore,
    bridge: &Bridge<BotClient, SqliteStore, SystemClock>,
    config: &HubConfig,
    state_dir: &Path,
    extra: &mut Vec<JoinHandle<()>>,
) {
    let topic = message.topic;
    if admin_only(&slash.name) && !config.admin_user_ids.contains(&message.sender_id) {
        say(bot, hub, topic, "admins only".to_owned()).await;
        return;
    }
    match slash.name.as_str() {
        CMD_STATUS => {
            let zalo = hub_zalo_health(hub).await;
            let bridge_health = bridge.health();
            say(
                bot,
                hub,
                topic,
                format!(
                    "zalo-bridge {} {}\nbridge {} {}",
                    state_name(zalo.state),
                    zalo.detail,
                    state_name(bridge_health.state),
                    bridge_health.detail
                ),
            )
            .await;
        }
        CMD_LOGIN => {
            write_event(
                hub,
                MODULE_ZALO_BRIDGE,
                LogLevel::Info,
                KIND_QR_RELOGIN,
                "qr login",
            )
            .await;
            match admin.start_qr().await {
                Ok(png) => match send_qr(bot, topic, state_dir, &png).await {
                    Ok(()) => {}
                    Err(err) => {
                        note_rate(matches!(err, hub_core::Error::RateLimited { .. }), hub).await;
                        say(bot, hub, topic, "could not send the qr".into()).await;
                    }
                },
                Err(_) => say(bot, hub, topic, "qr login failed".into()).await,
            }
            let waiter = Arc::clone(admin);
            let bot = bot.clone();
            let hub = hub.clone();
            extra.push(tokio::spawn(async move {
                match waiter.next_qr_state().await {
                    Ok(state) if state.state == "done" => {
                        say(&bot, &hub, topic, "zalo login finished".into()).await;
                    }
                    Ok(state) if state.state == "expired" => {
                        say(&bot, &hub, topic, "zalo qr expired".into()).await;
                    }
                    _ => {}
                }
            }));
        }
        CMD_CHECK => match bot.preflight().await {
            Ok(report) => say(bot, hub, topic, crate::hub::format_preflight(&report)).await,
            Err(err) => {
                note_rate(matches!(err, hub_tgbot::Error::RateLimited { .. }), hub).await;
                say(bot, hub, topic, "preflight failed".into()).await;
            }
        },
        CMD_TOPIC if slash.arg == CMD_LIST => {
            let topics = store.load_topics().await.unwrap_or_default();
            let mut lines = Vec::new();
            for mapping in topics.iter().take(TOPIC_LIST_CAP) {
                lines.push(format!("{} {}", mapping.topic.0, mapping.title));
            }
            if lines.is_empty() {
                lines.push("no topics".into());
            }
            say(bot, hub, topic, lines.join("\n")).await;
        }
        CMD_TOPIC if slash.arg == CMD_INFO => {
            let text = match store.conversation_for_topic(topic).await {
                Ok(Some(id)) => {
                    let title = store.title(&id).await.ok().flatten().unwrap_or_default();
                    format!("{title} {}", id)
                }
                _ => "no conversation for this topic".into(),
            };
            say(bot, hub, topic, text).await;
        }
        CMD_RECALL => {
            let Some(target) = message.reply_to else {
                say(bot, hub, topic, "reply to a message to recall".into()).await;
                return;
            };
            let _ = deliver(
                bridge,
                BridgeIncoming::Unsend {
                    topic,
                    tg_message: target,
                },
            )
            .await;
        }
        _ => say(bot, hub, topic, "command not available".into()).await,
    }
}

fn admin_only(name: &str) -> bool {
    name == CMD_LOGIN || name == CMD_RECALL
}

async fn send_qr(
    bot: &BotClient,
    topic: TopicId,
    state_dir: &Path,
    png_base64: &str,
) -> hub_core::Result<()> {
    let bytes = decode_base64(png_base64).map_err(|_| hub_core::Error::Adapter {
        detail: "qr png is not valid".into(),
    })?;
    let path = write_qr_png(state_dir, &bytes).map_err(|_| hub_core::Error::Adapter {
        detail: "cannot write the qr".into(),
    })?;
    let _ = render_qr_blocks(&bytes);
    let attachment = Attachment {
        kind: AttachmentKind::Photo,
        mime: Some("image/png".into()),
        size_bytes: u64::try_from(bytes.len()).ok(),
        source: AttachmentSource::LocalPath(path.display().to_string()),
        file_name: Some("zalo-qr.png".into()),
    };
    TopicSink::send_media(bot, topic, attachment, Some("Scan this Zalo QR".into()))
        .await
        .map(|_| ())
}

async fn hub_zalo_health(hub: &SharedHub) -> ModuleHealth {
    let hub = hub.clone();
    let found = tokio::task::spawn_blocking(move || hub.lock().reader().modules())
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default();
    found
        .into_iter()
        .find(|row| row.module == MODULE_ZALO_BRIDGE)
        .unwrap_or(ModuleHealth {
            module: MODULE_ZALO_BRIDGE.to_owned(),
            state: ModuleState::Unknown,
            detail: String::new(),
            updated_at_unix_secs: unix_now(),
        })
}

async fn say(bot: &BotClient, hub: &SharedHub, topic: TopicId, text: String) {
    let text = html_escape(&text);
    if let Err(err) = bot.send_text(topic, text, None).await {
        note_rate(matches!(err, hub_tgbot::Error::RateLimited { .. }), hub).await;
    }
}

async fn note_rate(limited: bool, hub: &SharedHub) {
    if limited {
        write_event(
            hub,
            MODULE_BRIDGE,
            LogLevel::Warn,
            KIND_RATE_LIMIT,
            "command",
        )
        .await;
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn state_name(state: ModuleState) -> &'static str {
    match state {
        ModuleState::Ok => "ok",
        ModuleState::Degraded => "degraded",
        ModuleState::Down => "down",
        ModuleState::Unknown => "unknown",
    }
}

async fn write_health(hub: &SharedHub, health: ModuleHealth) {
    let hub = hub.clone();
    let _ = tokio::task::spawn_blocking(move || hub.lock().record_health(&health)).await;
}

async fn write_event(hub: &SharedHub, module: &str, level: LogLevel, kind: &str, detail: &str) {
    let event = NewLogEvent {
        at_unix_secs: unix_now(),
        module: module.to_owned(),
        level,
        kind: kind.to_owned(),
        detail: Some(detail.to_owned()),
    };
    let hub = hub.clone();
    let _ = tokio::task::spawn_blocking(move || hub.lock().record_module_event(&event)).await;
}

fn unix_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

struct SettingsOffset {
    hub: SharedHub,
}

impl OffsetStore for SettingsOffset {
    async fn load(&self) -> hub_tgbot::Result<Option<i64>> {
        let hub = self.hub.clone();
        let value = tokio::task::spawn_blocking(move || hub.lock().setting(OFFSET_KEY))
            .await
            .map_err(|_| hub_tgbot::Error::Permanent { status: None })?
            .map_err(|_| hub_tgbot::Error::Permanent { status: None })?;
        match value {
            None => Ok(None),
            Some(text) => text
                .parse()
                .map(Some)
                .map_err(|_| hub_tgbot::Error::Permanent { status: None }),
        }
    }

    async fn save(&self, offset: i64) -> hub_tgbot::Result<()> {
        let hub = self.hub.clone();
        let text = offset.to_string();
        tokio::task::spawn_blocking(move || hub.lock().set_setting(OFFSET_KEY, &text))
            .await
            .map_err(|_| hub_tgbot::Error::Permanent { status: None })?
            .map_err(|_| hub_tgbot::Error::Permanent { status: None })?;
        Ok(())
    }
}

struct Gate {
    inner: ZaloAdapter,
    hub: SharedHub,
    skip_muted: bool,
}

impl NetworkAdapter for Gate {
    fn network(&self) -> hub_core::Network {
        NetworkAdapter::network(&self.inner)
    }

    async fn next_event(&self) -> hub_core::Result<Option<RemoteEvent>> {
        loop {
            let Some(event) = self.inner.next_event().await? else {
                return Ok(None);
            };
            if let Some(id) = conversation_of(&event)
                && self.is_muted(&id).await?
            {
                continue;
            }
            if let RemoteEvent::Message(message) = &event
                && self
                    .already_linked(&message.conversation, &message.message_id)
                    .await?
            {
                continue;
            }
            return Ok(Some(event));
        }
    }

    async fn send(&self, message: OutgoingMessage) -> hub_core::Result<hub_core::MessageRef> {
        NetworkAdapter::send(&self.inner, message).await
    }

    async fn react(
        &self,
        target: hub_core::MessageRef,
        emoji: String,
        added: bool,
    ) -> hub_core::Result<()> {
        NetworkAdapter::react(&self.inner, target, emoji, added).await
    }

    async fn unsend(&self, target: hub_core::MessageRef) -> hub_core::Result<()> {
        NetworkAdapter::unsend(&self.inner, target).await
    }

    async fn health(&self) -> hub_core::Result<ModuleHealth> {
        NetworkAdapter::health(&self.inner).await
    }
}

impl Gate {
    async fn is_muted(&self, id: &ConversationId) -> hub_core::Result<bool> {
        if !self.skip_muted {
            return Ok(false);
        }
        let id = id.clone();
        let hub = self.hub.clone();
        let found = tokio::task::spawn_blocking(move || hub.lock().conversation_muted(&id))
            .await
            .map_err(|_| hub_core::Error::Adapter {
                detail: "store task stopped".into(),
            })?
            .map_err(|_| hub_core::Error::Adapter {
                detail: "cannot read mute flag".into(),
            })?;
        Ok(found.unwrap_or(false))
    }

    async fn already_linked(
        &self,
        id: &ConversationId,
        message_id: &str,
    ) -> hub_core::Result<bool> {
        let id = id.clone();
        let message_id = message_id.to_owned();
        let hub = self.hub.clone();
        let found = tokio::task::spawn_blocking(move || {
            hub.lock().message_link_for_network(&id, &message_id)
        })
        .await
        .map_err(|_| hub_core::Error::Adapter {
            detail: "store task stopped".into(),
        })?
        .map_err(|_| hub_core::Error::Adapter {
            detail: "cannot read message map".into(),
        })?;
        Ok(found.is_some())
    }
}

fn conversation_of(event: &RemoteEvent) -> Option<ConversationId> {
    match event {
        RemoteEvent::Message(message) => Some(message.conversation.clone()),
        RemoteEvent::Edit(edit) => Some(edit.target.conversation.clone()),
        RemoteEvent::Unsend { target } | RemoteEvent::Reaction { target, .. } => {
            Some(target.conversation.clone())
        }
        RemoteEvent::ChatInfo { conversation, .. }
        | RemoteEvent::GroupEvent { conversation, .. } => Some(conversation.clone()),
        RemoteEvent::FriendRequest { .. } | RemoteEvent::SessionChanged { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_cover_the_wired_set() {
        assert_eq!(
            slash_command("/status@hub_bot"),
            Some(Slash {
                name: "status".into(),
                arg: String::new(),
            })
        );
        assert_eq!(
            slash_command("/topic list"),
            Some(Slash {
                name: "topic".into(),
                arg: "list".into(),
            })
        );
        assert_eq!(slash_command("hello").map(|slash| slash.name), None);
        assert!(admin_only("login"));
        assert!(admin_only("recall"));
        assert!(!admin_only("status"));
        assert!(!admin_only("checktopics"));
        assert!(!admin_only("topic"));
    }
}
