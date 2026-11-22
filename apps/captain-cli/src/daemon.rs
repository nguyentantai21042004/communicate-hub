//! `captain daemon`: the one process that owns the session.
//!
//! The main task owns the [`Connection`] and only reads updates from it, because the update
//! stream loses updates when a read is cancelled midway. Everything else holds a
//! [`captain_protocol::Handle`]: the pipeline, the socket, the web UI and the periodic jobs.
//! A dropped stream is reopened in this process. Cancelling a read is limited to shutdown,
//! a revoked session, and a keepalive probe that has already failed.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use captain_core::{ChatSummary, InboundEvent, Protocol};
use captain_protocol::{Connection, Error as ProtocolError, TelegramFailure, classify_failure};
use captain_store::Store;
use captain_vault::Vault;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{broadcast, mpsc};
use tokio::time::Instant;

use crate::app::{App, Config, Db, Health, Live, now_unix};
use crate::paths::Paths;
use crate::pipeline::{Pipeline, PipelineInput};
use crate::telemetry::{DropClass, StreamDrop, classify_error, record_drop, record_error};
use crate::{ipc_server, jobs, web};

const EVENT_BUFFER: usize = 256; // updates a slow `listen` client may fall behind by
const RETRY_AFTER: Duration = Duration::from_secs(5); // same-connection retry for a gap or a non-link catch-up error
const RECONNECT_INITIAL: Duration = Duration::from_secs(1); // jitter-free backoff start
const RECONNECT_MAX: Duration = Duration::from_secs(60); // jitter-free backoff cap
const RECONNECT_STABLE: Duration = Duration::from_secs(15 * 60); // reset backoff after this uptime
// grammers fetches updates itself after 15 minutes with none (NO_UPDATES_TIMEOUT). The daemon
// logs show a dead stream as `Operation timed out (os error 60)` on a read, not a quiet chat.
// One extra minute lets that fetch finish; a later failed probe is what reconnects.
const IDLE_TIMEOUT: Duration = Duration::from_secs(16 * 60);
const KEEPALIVE_PROBE: Duration = Duration::from_secs(60); // one grammers ping interval
const FLOOD_WAIT_CAP: Duration = Duration::from_secs(15 * 60); // longest same-connection sleep for one FLOOD_WAIT or SLOWMODE_WAIT
const FAILURE_WINDOW: Duration = Duration::from_secs(60); // non-link read failures inside this window count together
const FAILURE_THRESHOLD: u32 = 5; // failures inside FAILURE_WINDOW before status is degraded
const STOP_SIGNAL: &str = "signal";

/// Runs until SIGINT or SIGTERM. A dropped update stream is reopened in this process.
///
/// Missing API credentials or a logged-out session end the run with [`Ok`], so launchd stays
/// down. A startup failure such as a rejected request returns [`Err`].
///
/// # Errors
///
/// Returns an error when startup fails with `start_failed` or `listen_failed`, and when the
/// final session save fails.
pub async fn run(vault: Vault, paths: Paths, ui: web::Settings) -> Result<()> {
    let _lock = paths.lock_session()?;
    let config = Config::load(&paths).context("scope.toml or policy.toml is invalid")?;
    let store = Store::open(&paths.store())?;
    let paused = store.paused()?;
    let started_at = now_unix();
    let run_id = store.start_run(started_at)?;
    let db = Db::new(store);

    let profile = vault.profile().as_str().to_owned();
    let mut conn = match Connection::open(vault) {
        Ok(conn) => conn,
        Err(e @ captain_protocol::Error::NoApiCredentials(_)) => {
            tracing::error!(error = %e, "no API credentials; the daemon exits until they exist");
            let now = now_unix();
            db.run(move |s| s.stop_run(run_id, now, "no_credentials"))
                .await?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(signal) => signal,
        Err(error) => return stop(conn, &db, run_id, "start_failed", Err(error.into())).await,
    };
    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(signal) => signal,
        Err(error) => return stop(conn, &db, run_id, "start_failed", Err(error.into())).await,
    };
    let me = match open_session(&mut conn, &db, &mut terminate, &mut interrupt).await {
        Opened::Ready(me) => me,
        Opened::NotAuthorized => return stop(conn, &db, run_id, "not_authorized", Ok(())).await,
        Opened::Signal => return stop(conn, &db, run_id, STOP_SIGNAL, Ok(())).await,
        Opened::Failed { reason, error } => {
            return stop(conn, &db, run_id, reason, Err(error)).await;
        }
    };
    // Before the first read, so the catch-up that read triggers can reach every supergroup.
    // Updates that arrive meanwhile wait in grammers' queue.
    let cached = race_stop(&mut terminate, &mut interrupt, conn.cache_peers()).await;
    let Some(cached) = cached else {
        return stop(conn, &db, run_id, STOP_SIGNAL, Ok(())).await;
    };
    match cached {
        Ok(chats) => tracing::info!(chats, "chat list cached for catch-up"),
        Err(error @ ProtocolError::NotAuthorized(_)) => {
            tracing::error!(error = %error, "not logged in; the daemon exits until you log in");
            return stop(conn, &db, run_id, "not_authorized", Ok(())).await;
        }
        Err(error) => {
            tracing::warn!(error = %error, "could not cache the chat list; supergroup catch-up may skip gaps");
            record_error(&db, &error, now_unix()).await;
        }
    }
    let handle = conn.handle()?;
    let (events, _) = broadcast::channel(EVENT_BUFFER);
    let (pipeline_tx, pipeline_rx) = mpsc::unbounded_channel();

    let app = Arc::new(App {
        db: db.clone(),
        paths,
        profile,
        config: RwLock::new(config),
        paused: AtomicBool::new(paused),
        started_at,
        live: Some(Live {
            handle,
            me,
            events,
            pipeline: pipeline_tx.clone(),
            last_update_at: AtomicI64::new(0),
            open_bursts: AtomicUsize::new(0),
            caught_up: AtomicU64::new(0),
            unauthorized: AtomicBool::new(false),
            health: Health::new(),
        }),
        ui_url: RwLock::new(None),
    });

    let (socket, owner) = ipc_server::bind(&app)?;
    let web_listener = web::bind(&app, &ui).await?;
    let token = app.paths.ui_token()?;
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(Pipeline::new(Arc::clone(&app)).run(pipeline_rx));
    tasks.spawn(ipc_server::serve(Arc::clone(&app), socket, owner));
    tasks.spawn(web::serve(Arc::clone(&app), web_listener, token));
    tasks.spawn(jobs::run(Arc::clone(&app), run_id));
    tasks.spawn(crate::runner::run(Arc::clone(&app)));
    tracing::info!(
        chats = app.config().scope.entries().len(),
        paused,
        "daemon started"
    );

    let reason = {
        let Some(live) = app.live.as_ref() else {
            tasks.shutdown().await;
            let _ = std::fs::remove_file(app.paths.socket());
            return stop(conn, &db, run_id, STOP_SIGNAL, Ok(())).await;
        };
        let probe = live.handle.clone();
        supervise(
            &mut conn,
            &probe,
            Watch {
                now: now_unix,
                health: &live.health,
                unauthorized: &live.unauthorized,
                db: Some(&db),
            },
            |event| dispatch(&app, &pipeline_tx, event),
            async move {
                tokio::select! {
                    _ = interrupt.recv() => {}
                    _ = terminate.recv() => {}
                }
            },
        )
        .await
    };
    tasks.shutdown().await;
    let _ = std::fs::remove_file(app.paths.socket());
    stop(conn, &db, run_id, reason, Ok(())).await
}

enum Opened {
    Ready(ChatSummary),
    NotAuthorized,
    Signal,
    Failed {
        reason: &'static str,
        error: anyhow::Error,
    },
}

enum StartupError {
    Auth(ProtocolError),
    Link(ProtocolError),
    Fatal {
        reason: &'static str,
        error: anyhow::Error,
    },
}

/// `me` then `listen(true)`. A logged-out session stops. A dropped network backs off in
/// this process. Any other failure stops the run with the same reason the old startup used.
async fn open_session(
    conn: &mut Connection,
    db: &Db,
    terminate: &mut Signal,
    interrupt: &mut Signal,
) -> Opened {
    let mut delay = RECONNECT_INITIAL;
    loop {
        let Some(step) = race_stop(terminate, interrupt, establish(conn)).await else {
            return Opened::Signal;
        };
        match step {
            Ok(me) => return Opened::Ready(me),
            Err(StartupError::Auth(error)) => {
                // A clean exit, so launchd does not restart into a login loop. The owner has to
                // run `captain auth login` first.
                tracing::error!(error = %error, "not logged in; the daemon exits until you log in");
                return Opened::NotAuthorized;
            }
            Err(StartupError::Link(error)) => {
                tracing::warn!(error = %error, "opening the session failed; retrying");
                record_error(db, &error, now_unix()).await;
                if !startup_wait(delay, terminate, interrupt).await {
                    return Opened::Signal;
                }
                delay = next_delay(delay);
                if race_stop(terminate, interrupt, conn.restart_network())
                    .await
                    .is_none()
                {
                    return Opened::Signal;
                }
            }
            Err(StartupError::Fatal { reason, error }) => {
                return Opened::Failed { reason, error };
            }
        }
    }
}

async fn race_stop<T>(
    terminate: &mut Signal,
    interrupt: &mut Signal,
    fut: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = interrupt.recv() => None,
        _ = terminate.recv() => None,
        value = fut => Some(value),
    }
}

fn startup_error(error: ProtocolError, reason: &'static str) -> StartupError {
    if matches!(error, ProtocolError::NotAuthorized(_)) {
        StartupError::Auth(error)
    } else if classify_error(&error).is_link_failure() {
        StartupError::Link(error)
    } else {
        StartupError::Fatal {
            reason,
            error: error.into(),
        }
    }
}

/// `not_authorized` when startup must exit 0. `start_failed` and `listen_failed` are the
/// reasons launchd restarts.
#[cfg(test)]
fn startup_outcome(error: ProtocolError, reason: &'static str) -> &'static str {
    match startup_error(error, reason) {
        StartupError::Auth(_) => "not_authorized",
        StartupError::Link(_) => "link",
        StartupError::Fatal { reason, .. } => reason,
    }
}

async fn establish(conn: &mut Connection) -> Result<ChatSummary, StartupError> {
    if let Some(error) = conn.auth_key_block() {
        return Err(startup_error(error, "start_failed"));
    }
    let me = match conn.me().await {
        Ok(me) => me,
        Err(error) => return Err(startup_error(error, "start_failed")),
    };
    match conn.listen(true).await {
        Ok(()) => Ok(me),
        Err(error) => Err(startup_error(error, "listen_failed")),
    }
}

async fn startup_wait(delay: Duration, terminate: &mut Signal, interrupt: &mut Signal) -> bool {
    tokio::select! {
        biased;
        _ = interrupt.recv() => false,
        _ = terminate.recv() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// What the read loop needs from a connection. Tests use a script; production uses [`Connection`].
trait UpdateSource {
    /// Next update. `Ok(None)` means the stream ended.
    fn next_event(
        &mut self,
    ) -> impl Future<Output = Result<Option<InboundEvent>, SourceFail>> + Send;

    /// Stops the current network and starts it again. Does not open the update stream.
    fn reopen(&mut self) -> impl Future<Output = Result<(), SourceFail>> + Send;

    /// Opens the stream with catch-up, then caches peers.
    ///
    /// `Ok(true)` when both succeeded. `Ok(false)` when the stream is up and the peer cache
    /// failed. [`SourceFail::Down`] when the stream itself did not come back.
    /// [`SourceFail::Flood`] waits and retries this call on the same connection.
    fn catch_up(&mut self) -> impl Future<Output = Result<bool, SourceFail>> + Send;
}

/// A keepalive check that must not touch the update stream.
trait Probe {
    /// Whether the open connection is still authorized and reachable.
    fn probe(&self) -> impl Future<Output = ProbeResult> + Send;
}

#[derive(Clone)]
enum ProbeResult {
    /// Telegram answered the ping.
    Alive,
    /// The ping failed or timed out. The link is treated as dead.
    Dead,
    /// The session or auth key is dead. The string is the profile name, for the log.
    Auth(String),
}

enum SourceFail {
    /// Session revoked or not logged in. The string is the profile name, for the log.
    Auth(String),
    /// The update stream is not up.
    Down {
        class: DropClass,
        error: Option<ProtocolError>,
    },
    /// The stream is up and the peer cache did not fill. The gap stays open.
    Gap(ProtocolError),
    /// `FLOOD_WAIT` for `secs` seconds. Retry the same connection; do not reopen it.
    Flood { secs: u64 },
}

impl UpdateSource for Connection {
    async fn next_event(&mut self) -> Result<Option<InboundEvent>, SourceFail> {
        match Protocol::next_event(self).await {
            Ok(event) => Ok(event),
            Err(error) => Err(source_fail(error)),
        }
    }

    async fn reopen(&mut self) -> Result<(), SourceFail> {
        if let Some(error) = self.auth_key_block() {
            return Err(source_fail(error));
        }
        self.restart_network().await;
        Ok(())
    }

    async fn catch_up(&mut self) -> Result<bool, SourceFail> {
        if let Err(error) = self.listen(true).await {
            return Err(catch_up_fail(error, false));
        }
        match self.cache_peers().await {
            Ok(chats) => {
                tracing::info!(chats, "chat list cached for catch-up");
                Ok(true)
            }
            Err(error) => Err(catch_up_fail(error, true)),
        }
    }
}

impl Probe for captain_protocol::Handle {
    async fn probe(&self) -> ProbeResult {
        match tokio::time::timeout(KEEPALIVE_PROBE, self.ping()).await {
            Ok(Ok(())) => ProbeResult::Alive,
            Ok(Err(ProtocolError::NotAuthorized(profile))) => ProbeResult::Auth(profile),
            Ok(Err(error)) => match classify_failure(&error.to_string()) {
                TelegramFailure::NotAuthorized => ProbeResult::Auth(String::from("session")),
                TelegramFailure::FloodWait(_) | TelegramFailure::Other => {
                    tracing::warn!(error = %error, "keepalive probe failed");
                    ProbeResult::Dead
                }
            },
            Err(_elapsed) => {
                tracing::warn!("keepalive probe timed out");
                ProbeResult::Dead
            }
        }
    }
}

fn catch_up_fail(error: ProtocolError, stream_up: bool) -> SourceFail {
    if let ProtocolError::NotAuthorized(profile) = &error {
        return SourceFail::Auth(profile.clone());
    }
    match classify_failure(&error.to_string()) {
        TelegramFailure::NotAuthorized => SourceFail::Auth(String::from("session")),
        TelegramFailure::FloodWait(secs) => SourceFail::Flood { secs },
        TelegramFailure::Other if stream_up => SourceFail::Gap(error),
        TelegramFailure::Other => SourceFail::Down {
            class: classify_error(&error),
            error: Some(error),
        },
    }
}

fn source_fail(error: ProtocolError) -> SourceFail {
    if let ProtocolError::NotAuthorized(profile) = &error {
        return SourceFail::Auth(profile.clone());
    }
    match classify_failure(&error.to_string()) {
        TelegramFailure::NotAuthorized => SourceFail::Auth(String::from("session")),
        TelegramFailure::FloodWait(secs) => SourceFail::Flood { secs },
        TelegramFailure::Other => {
            let class = classify_error(&error);
            SourceFail::Down {
                class,
                error: Some(error),
            }
        }
    }
}

/// Non-link read failures inside [`FAILURE_WINDOW`], whatever the message text is.
/// A success clears the window and `degraded`.
struct Streak {
    at: Vec<Instant>,
    delay: Duration,
}

impl Streak {
    fn new() -> Self {
        Self {
            at: Vec::new(),
            delay: RECONNECT_INITIAL,
        }
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    /// Records one failure and returns how many fall inside the window.
    fn record(&mut self, now: Instant) -> u32 {
        let cutoff = now.checked_sub(FAILURE_WINDOW);
        self.at
            .retain(|stamp| cutoff.is_none_or(|cut| *stamp >= cut));
        self.at.push(now);
        u32::try_from(self.at.len()).unwrap_or(u32::MAX)
    }

    /// How long to wait before the next read. Doubles from 1 s up to 60 s.
    fn next_wait(&mut self) -> Duration {
        let wait = self.delay;
        self.delay = next_delay(self.delay);
        wait
    }
}

enum Read<T> {
    Event(T),
    Idle,
    Auth(String),
    Cancel,
}

#[derive(Clone, Copy)]
enum After {
    Resume {
        connected_at: Instant,
        delay: Duration,
    },
    Stop,
    Unauthorized,
}

/// Clock, status, and the metric store shared by one supervise loop.
struct Watch<'a, N> {
    now: N,
    health: &'a Health,
    unauthorized: &'a AtomicBool,
    db: Option<&'a Db>,
}

/// Reads until shutdown. A dropped stream reconnects here. An unauthorized session waits
/// for shutdown with the socket and the UI still up.
async fn supervise<S, P, N, D, C>(
    source: &mut S,
    probe: &P,
    watch: Watch<'_, N>,
    mut dispatch: D,
    cancel: C,
) -> &'static str
where
    S: UpdateSource,
    P: Probe,
    N: Fn() -> i64,
    D: FnMut(InboundEvent),
    C: Future<Output = ()>,
{
    let mut cancel = pin!(cancel);
    let mut connected_at = Instant::now();
    let mut delay = RECONNECT_INITIAL;
    let mut streak = Streak::new();
    loop {
        if watch.unauthorized.load(Ordering::Relaxed) {
            cancel.as_mut().await;
            return STOP_SIGNAL;
        }
        let read = next_read(source, probe, cancel.as_mut()).await;
        match read {
            Read::Cancel => return STOP_SIGNAL,
            Read::Event(Ok(Some(event))) => {
                streak.clear();
                watch.health.clear_degraded();
                dispatch(event);
            }
            Read::Event(Ok(None)) => {
                tracing::warn!("update stream ended");
                if drop_stream(
                    source,
                    &mut cancel,
                    &watch,
                    DropClass::ServerClosed,
                    &mut connected_at,
                    &mut delay,
                    &mut streak,
                )
                .await
                {
                    return STOP_SIGNAL;
                }
            }
            Read::Event(Err(SourceFail::Auth(profile))) | Read::Auth(profile) => {
                mark_unauthorized(watch.unauthorized, &profile);
            }
            Read::Event(Err(SourceFail::Flood { secs })) => {
                let _count = streak.record(Instant::now());
                let wait_for = note_flood(watch.health, secs);
                if !wait(wait_for, &mut cancel).await {
                    return STOP_SIGNAL;
                }
            }
            Read::Event(Err(SourceFail::Down { class, error })) => {
                note_proto(error.as_ref(), "reading updates failed; retrying");
                note_error(watch.db, error.as_ref(), (watch.now)()).await;
                if class.is_link_failure() {
                    if drop_stream(
                        source,
                        &mut cancel,
                        &watch,
                        class,
                        &mut connected_at,
                        &mut delay,
                        &mut streak,
                    )
                    .await
                    {
                        return STOP_SIGNAL;
                    }
                } else if other_failure(&mut streak, &watch, class, error.as_ref(), &mut cancel)
                    .await
                {
                    return STOP_SIGNAL;
                }
            }
            Read::Event(Err(SourceFail::Gap(error))) => {
                // `next_event` does not produce this. Treated as a same-connection retry.
                note_proto(Some(&error), "reading updates failed; retrying");
                note_error(watch.db, Some(&error), (watch.now)()).await;
                if !wait(RETRY_AFTER, &mut cancel).await {
                    return STOP_SIGNAL;
                }
            }
            Read::Idle => {
                tracing::warn!("no update for the idle timeout; reconnecting");
                if drop_stream(
                    source,
                    &mut cancel,
                    &watch,
                    DropClass::Timeout,
                    &mut connected_at,
                    &mut delay,
                    &mut streak,
                )
                .await
                {
                    return STOP_SIGNAL;
                }
            }
        }
    }
}

async fn other_failure<N, C>(
    streak: &mut Streak,
    watch: &Watch<'_, N>,
    class: DropClass,
    error: Option<&ProtocolError>,
    cancel: &mut Pin<&mut C>,
) -> bool
where
    N: Fn() -> i64,
    C: Future<Output = ()>,
{
    let count = streak.record(Instant::now());
    let wait_for = streak.next_wait();
    if count >= FAILURE_THRESHOLD {
        if count == FAILURE_THRESHOLD {
            tracing::warn!(
                class = class.as_str(),
                error = error.map(ToString::to_string).unwrap_or_default(),
                "repeated update failure; status is degraded"
            );
        }
        watch.health.mark_degraded(class.as_str());
    }
    !wait(wait_for, cancel).await
}

async fn drop_stream<S, N, C>(
    source: &mut S,
    cancel: &mut Pin<&mut C>,
    watch: &Watch<'_, N>,
    class: DropClass,
    connected_at: &mut Instant,
    delay: &mut Duration,
    streak: &mut Streak,
) -> bool
where
    S: UpdateSource,
    N: Fn() -> i64,
    C: Future<Output = ()>,
{
    let after = apply_reconnect(source, cancel, watch, class, *connected_at, *delay).await;
    if matches!(after, After::Resume { .. }) {
        streak.clear();
        watch.health.clear_degraded();
    }
    reconnect_failed(after, connected_at, delay)
}

fn reconnect_failed(after: After, connected_at: &mut Instant, delay: &mut Duration) -> bool {
    match after {
        After::Resume {
            connected_at: at,
            delay: next,
        } => {
            *connected_at = at;
            *delay = next;
            false
        }
        After::Stop => true,
        After::Unauthorized => false,
    }
}

/// One read, or an idle timeout whose probe failed.
///
/// The `next_event` future stays pinned when the probe succeeds. Dropping it would cancel
/// the update fetch grammers is in the middle of.
async fn next_read<S, P, C>(
    source: &mut S,
    probe: &P,
    mut cancel: Pin<&mut C>,
) -> Read<Result<Option<InboundEvent>, SourceFail>>
where
    S: UpdateSource,
    P: Probe,
    C: Future<Output = ()>,
{
    let mut next = pin!(source.next_event());
    let mut idle = pin!(tokio::time::sleep(IDLE_TIMEOUT));
    loop {
        tokio::select! {
            biased;
            () = cancel.as_mut() => return Read::Cancel,
            event = &mut next => return Read::Event(event),
            () = &mut idle => {}
        }
        tokio::select! {
            biased;
            () = cancel.as_mut() => return Read::Cancel,
            event = &mut next => return Read::Event(event),
            probed = probe.probe() => match probed {
                ProbeResult::Alive => {
                    // TELEGRAM: grammers NO_UPDATES_TIMEOUT is 15 min. It wakes the read to call
                    // updates.getDifference and yields no event. This reset leaves that read pinned.
                    idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                }
                ProbeResult::Dead => return Read::Idle,
                ProbeResult::Auth(profile) => return Read::Auth(profile),
            },
        }
    }
}

async fn apply_reconnect<S, N, C>(
    source: &mut S,
    cancel: &mut Pin<&mut C>,
    watch: &Watch<'_, N>,
    class: DropClass,
    connected_at: Instant,
    mut delay: Duration,
) -> After
where
    S: UpdateSource,
    N: Fn() -> i64,
    C: Future<Output = ()>,
{
    let detected = Instant::now();
    if connected_at.elapsed() >= RECONNECT_STABLE {
        delay = RECONNECT_INITIAL;
    }
    let dropped_at = (watch.now)();
    watch.health.begin_drop(dropped_at);
    tracing::warn!(
        class = class.as_str(),
        delay_secs = delay.as_secs(),
        "update stream dropped; reconnecting"
    );
    loop {
        if !wait(delay, cancel).await {
            return After::Stop;
        }
        let reopened = match stopped(cancel, source.reopen()).await {
            Err(()) => return After::Stop,
            Ok(result) => result,
        };
        match reopened {
            Ok(()) => {}
            Err(SourceFail::Auth(profile)) => {
                watch.health.reconnecting.store(false, Ordering::Relaxed);
                mark_unauthorized(watch.unauthorized, &profile);
                return After::Unauthorized;
            }
            Err(SourceFail::Flood { secs }) => {
                let wait_for = note_flood(watch.health, secs);
                tracing::warn!(secs, "flood wait during reopen; staying on this connection");
                if !wait(wait_for, cancel).await {
                    return After::Stop;
                }
            }
            Err(SourceFail::Down { class, error }) if !class.is_link_failure() => {
                note_proto(
                    error.as_ref(),
                    "reopening the update stream failed; retrying",
                );
                note_error(watch.db, error.as_ref(), (watch.now)()).await;
                if !wait(RETRY_AFTER, cancel).await {
                    return After::Stop;
                }
            }
            Err(SourceFail::Down { error, .. }) => {
                note_proto(
                    error.as_ref(),
                    "reopening the update stream failed; retrying",
                );
                note_error(watch.db, error.as_ref(), (watch.now)()).await;
                delay = next_delay(delay);
                continue;
            }
            Err(SourceFail::Gap(error)) => {
                note_proto(Some(&error), "reopening the update stream failed; retrying");
                note_error(watch.db, Some(&error), (watch.now)()).await;
                delay = next_delay(delay);
                continue;
            }
        }
        loop {
            let caught = match stopped(cancel, source.catch_up()).await {
                Err(()) => return After::Stop,
                Ok(result) => result,
            };
            match caught {
                Ok(gap_closed) => {
                    return restored(watch, detected, dropped_at, class, gap_closed, delay).await;
                }
                Err(SourceFail::Auth(profile)) => {
                    watch.health.reconnecting.store(false, Ordering::Relaxed);
                    mark_unauthorized(watch.unauthorized, &profile);
                    return After::Unauthorized;
                }
                Err(SourceFail::Flood { secs }) => {
                    let wait_for = note_flood(watch.health, secs);
                    tracing::warn!(
                        secs,
                        cap_secs = FLOOD_WAIT_CAP.as_secs(),
                        "catch-up hit a flood wait; retrying the same connection"
                    );
                    if !wait(wait_for, cancel).await {
                        return After::Stop;
                    }
                }
                Err(SourceFail::Gap(error)) => {
                    tracing::warn!(
                        error = %error,
                        "could not cache the chat list; supergroup catch-up may skip gaps"
                    );
                    note_error(watch.db, Some(&error), (watch.now)()).await;
                    return restored(watch, detected, dropped_at, class, false, delay).await;
                }
                Err(SourceFail::Down { class, error }) if class.is_link_failure() => {
                    note_proto(
                        error.as_ref(),
                        "reopening the update stream failed; retrying",
                    );
                    note_error(watch.db, error.as_ref(), (watch.now)()).await;
                    delay = next_delay(delay);
                    break;
                }
                Err(SourceFail::Down { error, .. }) => {
                    note_proto(
                        error.as_ref(),
                        "reopening the update stream failed; retrying",
                    );
                    note_error(watch.db, error.as_ref(), (watch.now)()).await;
                    if !wait(RETRY_AFTER, cancel).await {
                        return After::Stop;
                    }
                }
            }
        }
    }
}

async fn stopped<T, C: Future<Output = ()>>(
    cancel: &mut Pin<&mut C>,
    fut: impl Future<Output = T>,
) -> Result<T, ()> {
    tokio::select! {
        biased;
        () = cancel.as_mut() => Err(()),
        value = fut => Ok(value),
    }
}

async fn restored<N: Fn() -> i64>(
    watch: &Watch<'_, N>,
    detected: Instant,
    dropped_at: i64,
    class: DropClass,
    gap_closed: bool,
    delay: Duration,
) -> After {
    let secs = i64::try_from(detected.elapsed().as_secs()).unwrap_or(i64::MAX);
    watch.health.end_drop((watch.now)(), secs, gap_closed);
    if let Some(db) = watch.db {
        record_drop(
            db,
            StreamDrop {
                at: dropped_at,
                secs,
                class,
                gap_closed,
            },
        )
        .await;
    }
    tracing::info!(
        secs,
        class = class.as_str(),
        gap_closed,
        "update stream restored"
    );
    After::Resume {
        connected_at: Instant::now(),
        delay,
    }
}

async fn wait<C: Future<Output = ()>>(delay: Duration, cancel: &mut Pin<&mut C>) -> bool {
    tokio::select! {
        biased;
        () = cancel.as_mut() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

fn next_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(RECONNECT_MAX)
}

fn flood_wait(secs: u64) -> Duration {
    Duration::from_secs(secs).min(FLOOD_WAIT_CAP)
}

fn note_flood(health: &Health, secs: u64) -> Duration {
    let wait_for = flood_wait(secs);
    health.mark_flood(wait_for);
    wait_for
}

fn mark_unauthorized(unauthorized: &AtomicBool, profile: &str) {
    tracing::error!(profile, "session is no longer authorized; stopped reading");
    unauthorized.store(true, Ordering::Relaxed);
}

fn note_proto(error: Option<&ProtocolError>, message: &'static str) {
    match error {
        Some(error) => tracing::warn!(error = %error, "{message}"),
        None => tracing::warn!("{message}"),
    }
}

async fn note_error(db: Option<&Db>, error: Option<&ProtocolError>, now: i64) {
    if let (Some(db), Some(error)) = (db, error) {
        record_error(db, error, now).await;
    }
}

fn dispatch(app: &App, pipeline: &mpsc::UnboundedSender<PipelineInput>, event: InboundEvent) {
    let Some(live) = &app.live else {
        return;
    };
    live.last_update_at
        .store(event.received_at_unix, Ordering::Relaxed);
    let _ = live.events.send(event.clone());
    if pipeline
        .send(PipelineInput::Event(Box::new(event)))
        .is_err()
    {
        tracing::error!("pipeline stopped; updates are no longer processed");
    }
}

async fn stop(
    conn: Connection,
    db: &Db,
    run_id: i64,
    reason: &'static str,
    result: Result<()>,
) -> Result<()> {
    let now = now_unix();
    if let Err(e) = db.run(move |s| s.stop_run(run_id, now, reason)).await {
        tracing::warn!(error = %e, "could not record the end of this run");
    }
    let closed = conn.close().await;
    tracing::info!(reason, "daemon stopped");
    result?;
    closed?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use captain_core::{EventKind, InboundEvent};
    use captain_protocol::Error as ProtocolError;
    use captain_store::{MetricKind, Store};
    use tokio::sync::{Notify, oneshot};
    use tokio::time::Instant;

    use super::{
        FLOOD_WAIT_CAP, IDLE_TIMEOUT, ProbeResult, RECONNECT_INITIAL, RECONNECT_STABLE,
        RETRY_AFTER, STOP_SIGNAL, SourceFail, UpdateSource, supervise,
    };
    use crate::app::{Db, Health};
    use crate::telemetry::DropClass;

    const NOW: i64 = 1_700_000_000;
    const BACKOFF_TOTAL: u64 = 243; // 1+2+4+8+16+32+60+60+60

    enum Script {
        Event(Box<InboundEvent>),
        End,
        Auth,
        Down(DropClass),
        Hang,
        Wait(Arc<Notify>),
        Delay(Duration),
        Raw(String),
    }

    enum CatchUp {
        Done(bool),
        Auth,
        Flood(u64),
        Hang,
    }

    struct Fake {
        reads: VecDeque<Script>,
        reopens: VecDeque<Result<(), SourceFail>>,
        catch_ups: VecDeque<CatchUp>,
        reopen_at: Arc<Mutex<Vec<Instant>>>,
        catch_up_calls: Arc<AtomicUsize>,
        read_calls: Arc<AtomicUsize>,
    }

    impl Fake {
        fn new(
            reads: Vec<Script>,
            reopens: Vec<Result<(), SourceFail>>,
            catch_ups: Vec<CatchUp>,
            reopen_at: Arc<Mutex<Vec<Instant>>>,
            catch_up_calls: Arc<AtomicUsize>,
            read_calls: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                reads: reads.into(),
                reopens: reopens.into(),
                catch_ups: catch_ups.into(),
                reopen_at,
                catch_up_calls,
                read_calls,
            }
        }
    }

    impl UpdateSource for Fake {
        async fn next_event(&mut self) -> Result<Option<InboundEvent>, SourceFail> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            loop {
                match self.reads.pop_front() {
                    Some(Script::Event(event)) => return Ok(Some(*event)),
                    Some(Script::End) => return Ok(None),
                    Some(Script::Auth) => return Err(SourceFail::Auth("test".into())),
                    Some(Script::Down(class)) => {
                        return Err(SourceFail::Down { class, error: None });
                    }
                    Some(Script::Hang) => std::future::pending().await,
                    Some(Script::Wait(notify)) => notify.notified().await,
                    Some(Script::Delay(duration)) => tokio::time::sleep(duration).await,
                    Some(Script::Raw(message)) => {
                        return Err(super::source_fail(captain_protocol::Error::Rpc {
                            request: "updates.getDifference",
                            message,
                        }));
                    }
                    None => {
                        return Err(SourceFail::Down {
                            class: DropClass::Other,
                            error: None,
                        });
                    }
                }
            }
        }

        async fn reopen(&mut self) -> Result<(), SourceFail> {
            self.reopen_at.lock().unwrap().push(Instant::now());
            self.reopens.pop_front().unwrap_or(Err(SourceFail::Down {
                class: DropClass::Other,
                error: None,
            }))
        }

        async fn catch_up(&mut self) -> Result<bool, SourceFail> {
            self.catch_up_calls.fetch_add(1, Ordering::Relaxed);
            match self.catch_ups.pop_front() {
                Some(CatchUp::Done(closed)) => Ok(closed),
                Some(CatchUp::Auth) => Err(SourceFail::Auth("revoked".into())),
                Some(CatchUp::Flood(secs)) => Err(SourceFail::Flood { secs }),
                Some(CatchUp::Hang) => std::future::pending().await,
                None => Ok(false),
            }
        }
    }

    struct FlagProbe {
        answer: ProbeResult,
        calls: Arc<AtomicUsize>,
    }

    impl super::Probe for FlagProbe {
        async fn probe(&self) -> ProbeResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.answer.clone()
        }
    }

    struct Session {
        task: tokio::task::JoinHandle<&'static str>,
        health: Arc<Health>,
        unauthorized: Arc<AtomicBool>,
        reopens: Arc<Mutex<Vec<Instant>>>,
        catch_ups: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
        probes: Arc<AtomicUsize>,
        stop: Arc<Notify>,
    }

    fn spawn_session(
        reads: Vec<Script>,
        reopens_script: Vec<Result<(), SourceFail>>,
        catches: Vec<CatchUp>,
        probe: ProbeResult,
        db: Option<Db>,
    ) -> Session {
        let health = Arc::new(Health::new());
        let unauthorized = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(Notify::new());
        let probes = Arc::new(AtomicUsize::new(0));
        let reopens = Arc::new(Mutex::new(Vec::new()));
        let catch_ups = Arc::new(AtomicUsize::new(0));
        let read_calls = Arc::new(AtomicUsize::new(0));
        let mut fake = Fake::new(
            reads,
            reopens_script,
            catches,
            Arc::clone(&reopens),
            Arc::clone(&catch_ups),
            Arc::clone(&read_calls),
        );
        let health_task = Arc::clone(&health);
        let unauthorized_task = Arc::clone(&unauthorized);
        let stop_wait = Arc::clone(&stop);
        let stop_fire = Arc::clone(&stop);
        let probe = FlagProbe {
            answer: probe,
            calls: Arc::clone(&probes),
        };
        let task = tokio::spawn(async move {
            supervise(
                &mut fake,
                &probe,
                super::Watch {
                    now: || NOW,
                    health: &health_task,
                    unauthorized: &unauthorized_task,
                    db: db.as_ref(),
                },
                move |_| stop_fire.notify_one(),
                async move { stop_wait.notified().await },
            )
            .await
        });
        Session {
            task,
            health,
            unauthorized,
            reopens,
            catch_ups,
            reads: read_calls,
            probes,
            stop,
        }
    }

    fn sample_event() -> InboundEvent {
        InboundEvent {
            received_at_unix: 1,
            kind: EventKind::Other,
            raw_type: "test".into(),
            raw: serde_json::Value::Null,
        }
    }

    fn link_down() -> SourceFail {
        SourceFail::Down {
            class: DropClass::ServerClosed,
            error: None,
        }
    }

    async fn until(mut ready: impl FnMut() -> bool) {
        for _ in 0..100 {
            if ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert!(ready(), "condition was not reached");
    }

    async fn hold_clock() -> (tokio::task::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        let _ = started_rx.await;
        (blocker, release_tx)
    }

    fn gaps(times: &[Instant]) -> Vec<Duration> {
        times
            .windows(2)
            .map(|pair| pair[1].saturating_duration_since(pair[0]))
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn stream_end_reconnects_once_and_records_the_drop() {
        let (blocker, release) = hold_clock().await;
        let db = Db::new(Store::open_in_memory().unwrap());
        let session = spawn_session(
            vec![Script::End, Script::Event(Box::new(sample_event()))],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            Some(db.clone()),
        );
        until(|| session.health.reconnecting.load(Ordering::Relaxed)).await;
        assert_eq!(session.health.reconnects_24h(NOW), 0);
        assert!(session.health.gap_unfilled.load(Ordering::Relaxed));
        assert_eq!(session.health.last_drop_at.load(Ordering::Relaxed), NOW);
        assert_eq!(session.health.last_drop_secs.load(Ordering::Relaxed), -1);
        assert!(session.reopens.lock().unwrap().is_empty());

        release.send(()).unwrap();
        blocker.await.unwrap();
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert!(!session.health.reconnecting.load(Ordering::Relaxed));
        assert_eq!(session.health.reconnects_24h(NOW), 1);
        assert_eq!(session.health.last_drop_at.load(Ordering::Relaxed), NOW);
        assert_eq!(session.health.last_drop_secs.load(Ordering::Relaxed), 1);
        assert!(!session.health.gap_unfilled.load(Ordering::Relaxed));
        let count = db
            .run(|store| store.count_metrics(MetricKind::StreamDrop, 0))
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_failures_back_off_and_cap() {
        let start = Instant::now();
        let mut attempts = Vec::new();
        for _ in 0..8 {
            attempts.push(Err(link_down()));
        }
        attempts.push(Ok(()));
        let session = spawn_session(
            vec![
                Script::Down(DropClass::Timeout),
                Script::Event(Box::new(sample_event())),
            ],
            attempts,
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(Duration::from_secs(BACKOFF_TOTAL + 1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        let times = session.reopens.lock().unwrap().clone();
        assert_eq!(times.len(), 9);
        assert_eq!(
            times[0].saturating_duration_since(start),
            Duration::from_secs(1)
        );
        let expected: Vec<Duration> = [2, 4, 8, 16, 32, 60, 60, 60]
            .into_iter()
            .map(Duration::from_secs)
            .collect();
        assert_eq!(gaps(&times), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn stable_uptime_resets_the_backoff() {
        let start = Instant::now();
        let session = spawn_session(
            vec![
                Script::End,
                Script::Delay(RECONNECT_STABLE),
                Script::End,
                Script::Event(Box::new(sample_event())),
            ],
            vec![Err(link_down()), Ok(()), Ok(())],
            vec![CatchUp::Done(true), CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        // 1s fail, 2s success, 15 min up, 1s after the reset.
        tokio::time::sleep(Duration::from_secs(3 + 15 * 60 + 1 + 1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 2);
        let times = session.reopens.lock().unwrap().clone();
        assert_eq!(times.len(), 3);
        assert_eq!(
            times[0].saturating_duration_since(start),
            Duration::from_secs(1)
        );
        assert_eq!(
            gaps(&times),
            vec![
                Duration::from_secs(2),
                RECONNECT_STABLE + Duration::from_secs(1),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn auth_failure_does_not_reconnect() {
        let session = spawn_session(vec![Script::Auth], vec![], vec![], ProbeResult::Alive, None);
        until(|| session.unauthorized.load(Ordering::Relaxed)).await;
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert_eq!(session.health.reconnects_24h(NOW), 0);
        assert!(!session.task.is_finished());
        let seen = requests(&session);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(requests(&session), seen);
        assert!(!session.task.is_finished());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn auth_during_reconnect_does_not_retry() {
        let session = spawn_session(
            vec![Script::End],
            vec![Err(SourceFail::Auth("revoked".into()))],
            vec![],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.unauthorized.load(Ordering::Relaxed));
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert_eq!(session.health.reconnects_24h(NOW), 0);
        assert!(!session.health.reconnecting.load(Ordering::Relaxed));
        assert!(session.health.gap_unfilled.load(Ordering::Relaxed));
        assert_eq!(session.health.last_drop_secs.load(Ordering::Relaxed), -1);
        assert!(!session.task.is_finished());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn peer_cache_miss_leaves_the_gap_open() {
        let session = spawn_session(
            vec![Script::End, Script::Event(Box::new(sample_event()))],
            vec![Ok(())],
            vec![CatchUp::Done(false)],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(session.health.reconnects_24h(NOW), 1);
        assert!(session.health.gap_unfilled.load(Ordering::Relaxed));
        assert_eq!(session.health.last_drop_secs.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn rpc_errors_retry_without_reopening() {
        let session = spawn_session(
            vec![
                Script::Down(DropClass::Other),
                Script::Event(Box::new(sample_event())),
            ],
            vec![],
            vec![],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RETRY_AFTER + Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert_eq!(session.health.reconnects_24h(NOW), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_probe_keeps_the_open_read() {
        let gate = Arc::new(Notify::new());
        let session = spawn_session(
            vec![
                Script::Wait(Arc::clone(&gate)),
                Script::Event(Box::new(sample_event())),
            ],
            vec![],
            vec![],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert!(!session.task.is_finished());
        gate.notify_one();
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert!(session.reopens.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dead_probe_reconnects() {
        let session = spawn_session(
            vec![Script::Hang, Script::Event(Box::new(sample_event()))],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Dead,
            None,
        );
        tokio::time::sleep(IDLE_TIMEOUT + RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(session.health.reconnects_24h(NOW), 1);
        // The idle wait is before the drop is noticed. The recorded length is the reconnect.
        assert_eq!(session.health.last_drop_secs.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn catch_up_auth_makes_no_further_request() {
        let session = spawn_session(
            vec![Script::End],
            vec![Ok(())],
            vec![CatchUp::Auth],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.unauthorized.load(Ordering::Relaxed));
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(session.health.reconnects_24h(NOW), 0);
        assert!(!session.health.reconnecting.load(Ordering::Relaxed));
        let seen = requests(&session);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(requests(&session), seen);
        assert!(!session.task.is_finished());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn catch_up_flood_retries_without_reopen() {
        let session = spawn_session(
            vec![Script::End],
            vec![Ok(())],
            vec![CatchUp::Flood(31), CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(30)).await;
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 2);
        assert!(!session.health.reconnecting.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn catch_up_flood_is_capped() {
        let session = spawn_session(
            vec![Script::End],
            vec![Ok(())],
            vec![
                CatchUp::Flood(FLOOD_WAIT_CAP.as_secs() + 100),
                CatchUp::Done(true),
            ],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + FLOOD_WAIT_CAP - Duration::from_secs(1)).await;
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 2);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_backoff_does_not_reopen() {
        let session = spawn_session(
            vec![Script::End],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        until(|| session.health.reconnecting.load(Ordering::Relaxed)).await;
        assert!(session.reopens.lock().unwrap().is_empty());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_during_catch_up_does_not_reopen_again() {
        let session = spawn_session(
            vec![Script::End],
            vec![Ok(())],
            vec![CatchUp::Hang],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert!(!session.task.is_finished());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn probe_auth_does_not_reconnect() {
        let session = spawn_session(
            vec![Script::Hang],
            vec![],
            vec![],
            ProbeResult::Auth("revoked".into()),
            None,
        );
        tokio::time::sleep(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        assert!(session.unauthorized.load(Ordering::Relaxed));
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert_eq!(session.probes.load(Ordering::Relaxed), 1);
        assert_eq!(session.reads.load(Ordering::Relaxed), 1);
        let seen = requests(&session);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(requests(&session), seen);
        assert!(!session.task.is_finished());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn raw_session_revoked_stops_without_another_request() {
        raw_auth_stops("rpc error 401: SESSION_REVOKED").await;
    }

    #[tokio::test(start_paused = true)]
    async fn raw_transport_404_stops_without_another_request() {
        raw_auth_stops("bad status (negative length -404)").await;
    }

    async fn raw_auth_stops(message: &str) {
        let session = spawn_session(
            vec![
                Script::Raw(message.to_owned()),
                Script::Event(Box::new(sample_event())),
            ],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        until(|| session.unauthorized.load(Ordering::Relaxed)).await;
        assert!(
            !session.health.degraded.load(Ordering::Relaxed),
            "{message}"
        );
        assert!(session.reopens.lock().unwrap().is_empty(), "{message}");
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0, "{message}");
        assert_eq!(session.reads.load(Ordering::Relaxed), 1, "{message}");
        let seen = requests(&session);
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(requests(&session), seen, "{message}");
        assert!(!session.task.is_finished(), "{message}");
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn raw_flood_wait_retries_on_the_same_connection() {
        let session = spawn_session(
            vec![
                Script::Raw("rpc error 420: FLOOD_WAIT (value: 31)".into()),
                Script::Event(Box::new(sample_event())),
            ],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(session.reads.load(Ordering::Relaxed), 1);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert!(session.health.degraded.load(Ordering::Relaxed));
        assert_eq!(
            session.health.degraded_reason().as_deref(),
            Some("flood_wait")
        );
        assert_eq!(session.health.connection(false), "degraded");
        assert_eq!(session.health.flood_wait_secs(), Some(25));
        assert!(!session.task.is_finished());
        tokio::time::sleep(Duration::from_secs(26)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.reads.load(Ordering::Relaxed), 2);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 0);
        assert!(!session.health.degraded.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn raw_slowmode_waits_then_retries_without_reopen() {
        let session = spawn_session(
            vec![
                Script::Raw("rpc error 420: SLOWMODE_WAIT (value: 12)".into()),
                Script::Event(Box::new(sample_event())),
            ],
            vec![],
            vec![],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(Duration::from_secs(11)).await;
        assert_eq!(session.reads.load(Ordering::Relaxed), 1);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert!(session.health.degraded.load(Ordering::Relaxed));
        assert_eq!(
            session.health.degraded_reason().as_deref(),
            Some("flood_wait")
        );
        assert_eq!(session.health.flood_wait_secs(), Some(1));
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.reads.load(Ordering::Relaxed), 2);
        assert!(session.reopens.lock().unwrap().is_empty());
        assert!(!session.health.degraded.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn raw_link_error_reopens_instead_of_degrading() {
        let session = spawn_session(
            vec![
                Script::Raw("request error: Operation timed out (os error 60)".into()),
                Script::Event(Box::new(sample_event())),
            ],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            ProbeResult::Alive,
            None,
        );
        tokio::time::sleep(RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(session.reopens.lock().unwrap().len(), 1);
        assert_eq!(session.catch_ups.load(Ordering::Relaxed), 1);
        assert!(!session.health.degraded.load(Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn other_failures_degrade_after_five_and_a_success_clears_it() {
        for message in OTHER_FAILURES {
            assert_other_degrades(message).await;
        }
    }

    const OTHER_FAILURES: &[&str] = &[
        "rpc error 400: PEER_ID_INVALID",
        "rpc error 500: INTERNAL_SERVER_ERROR",
        "rpc error 303: USER_MIGRATE (value: 2)",
        "bad status (negative length -429)",
        "Connection refused (os error 61)",
        "Broken pipe (os error 32)",
        "network down",
        "request error: session error",
        "request error: invalid dc",
        "bad server auth key (got 1, expected 2)",
        "not a grammers error",
    ];

    async fn assert_other_degrades(message: &str) {
        let reads = (0..5).map(|_| Script::Raw(message.to_owned())).collect();
        let session = spawn_session(reads, vec![], vec![], ProbeResult::Alive, None);
        tokio::time::sleep(Duration::from_secs(14)).await;
        assert!(
            !session.health.degraded.load(Ordering::Relaxed),
            "degraded before the fifth failure: {message}"
        );
        assert_eq!(session.reads.load(Ordering::Relaxed), 4, "{message}");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            session.health.degraded.load(Ordering::Relaxed),
            "stayed connected: {message}"
        );
        assert_eq!(
            session.health.degraded_reason().as_deref(),
            Some("other"),
            "{message}"
        );
        assert_eq!(session.health.connection(false), "degraded", "{message}");
        assert!(
            !session.health.reconnecting.load(Ordering::Relaxed),
            "{message}"
        );
        assert!(session.reopens.lock().unwrap().is_empty(), "{message}");
        assert_eq!(session.reads.load(Ordering::Relaxed), 5, "{message}");
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_clears_degraded() {
        let mut reads: Vec<Script> = (0..5)
            .map(|_| Script::Raw("rpc error 400: PEER_ID_INVALID".into()))
            .collect();
        reads.push(Script::Event(Box::new(sample_event())));
        let session = spawn_session(reads, vec![], vec![], ProbeResult::Alive, None);
        tokio::time::sleep(Duration::from_secs(16)).await;
        assert!(session.health.degraded.load(Ordering::Relaxed));
        assert!(session.reopens.lock().unwrap().is_empty());
        tokio::time::sleep(Duration::from_secs(16)).await;
        assert!(session.task.is_finished());
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
        assert!(!session.health.degraded.load(Ordering::Relaxed));
        assert!(session.health.degraded_reason().is_none());
        assert_eq!(session.health.connection(false), "connected");
        assert!(session.reopens.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn alternating_errors_reach_degraded() {
        let messages = [
            "rpc error 400: PEER_ID_INVALID",
            "rpc error 500: INTERNAL_SERVER_ERROR",
            "rpc error 303: USER_MIGRATE (value: 2)",
            "bad status (negative length -429)",
            "not a grammers error",
        ];
        let reads = messages
            .into_iter()
            .map(|message| Script::Raw(message.to_owned()))
            .collect();
        let session = spawn_session(reads, vec![], vec![], ProbeResult::Alive, None);
        tokio::time::sleep(Duration::from_secs(14)).await;
        assert!(!session.health.degraded.load(Ordering::Relaxed));
        assert_eq!(session.reads.load(Ordering::Relaxed), 4);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(session.health.degraded.load(Ordering::Relaxed));
        assert_eq!(session.health.degraded_reason().as_deref(), Some("other"));
        assert_eq!(session.health.connection(false), "degraded");
        assert!(session.reopens.lock().unwrap().is_empty());
        session.stop.notify_one();
        until(|| session.task.is_finished()).await;
        assert_eq!(session.task.await.unwrap(), STOP_SIGNAL);
    }

    #[test]
    fn startup_auth_key_conflict_exits_not_authorized() {
        let conflict = ProtocolError::NotAuthorized("default".into());
        assert_eq!(
            super::startup_outcome(conflict, "start_failed"),
            "not_authorized"
        );
        let listen = ProtocolError::NotAuthorized("default".into());
        assert_eq!(
            super::startup_outcome(listen, "listen_failed"),
            "not_authorized"
        );
        let swallowed = ProtocolError::Rpc {
            request: "users.getUsers",
            message: "session error".into(),
        };
        assert_eq!(
            super::startup_outcome(swallowed, "start_failed"),
            "start_failed"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn home_sender_gone_reconnects_without_minting_a_key() {
        let mints = Arc::new(AtomicUsize::new(0));
        let open = Arc::new(AtomicBool::new(false));
        let probes = Arc::new(AtomicUsize::new(0));
        let probe = ClosedSenderProbe {
            open: Arc::clone(&open),
            mints: Arc::clone(&mints),
            calls: Arc::clone(&probes),
        };
        let health = Arc::new(Health::new());
        let unauthorized = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(Notify::new());
        let reopens = Arc::new(Mutex::new(Vec::new()));
        let catch_ups = Arc::new(AtomicUsize::new(0));
        let read_calls = Arc::new(AtomicUsize::new(0));
        let mut fake = Fake::new(
            vec![Script::Hang, Script::Event(Box::new(sample_event()))],
            vec![Ok(())],
            vec![CatchUp::Done(true)],
            Arc::clone(&reopens),
            Arc::clone(&catch_ups),
            Arc::clone(&read_calls),
        );
        let health_task = Arc::clone(&health);
        let unauthorized_task = Arc::clone(&unauthorized);
        let stop_wait = Arc::clone(&stop);
        let stop_fire = Arc::clone(&stop);
        let task = tokio::spawn(async move {
            supervise(
                &mut fake,
                &probe,
                super::Watch {
                    now: || NOW,
                    health: &health_task,
                    unauthorized: &unauthorized_task,
                    db: None,
                },
                move |_| stop_fire.notify_one(),
                async move { stop_wait.notified().await },
            )
            .await
        });
        tokio::time::sleep(IDLE_TIMEOUT + RECONNECT_INITIAL + Duration::from_secs(1)).await;
        assert!(task.is_finished());
        assert_eq!(task.await.unwrap(), STOP_SIGNAL);
        assert_eq!(mints.load(Ordering::Relaxed), 0);
        assert!(!open.load(Ordering::Relaxed));
        assert_eq!(probes.load(Ordering::Relaxed), 1);
        assert_eq!(reopens.lock().unwrap().len(), 1);
        assert_eq!(catch_ups.load(Ordering::Relaxed), 1);
        assert_eq!(health.reconnects_24h(NOW), 1);
        assert!(!unauthorized.load(Ordering::Relaxed));
    }

    /// Models `Handle::ping`: a closed home sender is a dead link and must not mint a key.
    struct ClosedSenderProbe {
        open: Arc<AtomicBool>,
        mints: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
    }

    impl super::Probe for ClosedSenderProbe {
        async fn probe(&self) -> ProbeResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.open.load(Ordering::Relaxed) {
                self.mints.fetch_add(1, Ordering::Relaxed);
                ProbeResult::Alive
            } else {
                ProbeResult::Dead
            }
        }
    }

    fn requests(session: &Session) -> (usize, usize, usize, usize) {
        (
            session.reads.load(Ordering::Relaxed),
            session.reopens.lock().unwrap().len(),
            session.catch_ups.load(Ordering::Relaxed),
            session.probes.load(Ordering::Relaxed),
        )
    }
}
