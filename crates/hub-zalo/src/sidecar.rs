//! Spawns one `zalo-node` process and speaks the sidecar contract on its pipes.
//!
//! The child is started with [`std::process::Command`] via Tokio. The command is never a shell.
//! An unexpected exit fails the calls that were in flight, then waits with exponential backoff.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::limits::{
    CRASH_LOOP_CAP, DEFAULT_REQUEST_TIMEOUT, EVENT_QUEUE_CAPACITY, MAX_STDOUT_LINE_BYTES,
    RESTART_INITIAL_BACKOFF, RESTART_MAX_BACKOFF, RESTART_WINDOW,
};
use crate::protocol::{
    SESSION_DISCONNECTED, SidecarEvent, StdoutFrame, parse_stdout, string_field,
};
use crate::retention::cleanup_media_dir;

const HEALTH_MODULE: &str = "zalo-bridge";
const LISTENER_UP: &str = "up";

/// How to start the sidecar. Paths are passed as arguments and environment, not a shell string.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Executable, usually `node`.
    pub program: OsString,
    /// Arguments, usually the path of `dist/main.js`.
    pub args: Vec<OsString>,
    /// Value of `ZALO_CREDENTIALS_PATH`. The supervisor does not read the file.
    pub credentials_path: PathBuf,
    /// Value of `ZALO_DATA_DIR`. Media cleanup looks at `<data_dir>/media`.
    pub data_dir: PathBuf,
    /// How long [`Sidecar::call`] waits for a matching response.
    pub request_timeout: Duration,
    /// Delay after the first unexpected exit.
    pub restart_initial: Duration,
    /// Cap on the restart delay.
    pub restart_max: Duration,
    /// Unexpected exits in a row before calls fail and health goes down.
    pub crash_loop_cap: u32,
}

impl SidecarConfig {
    /// Builds a config with the default timeouts from [`crate::limits`].
    pub fn new(
        program: impl Into<OsString>,
        args: Vec<OsString>,
        credentials_path: PathBuf,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            program: program.into(),
            args,
            credentials_path,
            data_dir,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            restart_initial: RESTART_INITIAL_BACKOFF,
            restart_max: RESTART_MAX_BACKOFF,
            crash_loop_cap: CRASH_LOOP_CAP,
        }
    }
}

/// Supervisor phase visible to tests and health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// [`Sidecar::launch`] has not spawned a process yet.
    Starting,
    /// A process is running.
    Running,
    /// Waiting before the next spawn.
    BackingOff,
    /// [`SidecarConfig::crash_loop_cap`] was reached.
    CrashLoop,
    /// Shutdown completed and no restart will happen.
    Stopped,
}

/// Snapshot used to build [`hub_core::ModuleHealth`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarStatus {
    /// The listener event last said `up`.
    pub listener_up: bool,
    /// Last session state token from the sidecar.
    pub session: String,
    /// Unix seconds of the last respawn. `0` before the first restart.
    pub last_restart_unix_secs: i64,
    /// Respawns inside the last 24 hours. The initial spawn is not counted.
    pub restarts_24h: u64,
    /// Processes started, including the initial one.
    pub spawn_count: u64,
    /// The crash-loop cap was hit.
    pub crash_loop: bool,
    /// What the supervisor is doing.
    pub phase: RunPhase,
}

struct Status {
    listener_up: bool,
    session: String,
    last_restart_unix_secs: i64,
    restarts: Vec<tokio::time::Instant>,
    spawn_count: u64,
    crash_loop: bool,
    phase: RunPhase,
}

struct Call {
    id: u64,
    method: String,
    params: Value,
    reply: oneshot::Sender<Result<Value>>,
}

struct Shared {
    status: Mutex<Status>,
    events: broadcast::Sender<SidecarEvent>,
    keepalive: Mutex<broadcast::Receiver<SidecarEvent>>,
    shutdown: watch::Sender<bool>,
    phase: watch::Sender<RunPhase>,
}

/// One running sidecar supervisor.
///
/// Drop aborts the supervisor task. The child is killed because it was spawned with
/// `kill_on_drop`. Prefer [`Sidecar::shutdown`] when the caller can await.
pub struct Sidecar {
    shared: std::sync::Arc<Shared>,
    calls: mpsc::UnboundedSender<Call>,
    inbound: Mutex<Option<mpsc::UnboundedReceiver<Call>>>,
    config: SidecarConfig,
    next_id: AtomicU64,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Sidecar {
    /// Builds the supervisor without starting a process.
    ///
    /// Subscribe with [`Sidecar::subscribe`] before [`Sidecar::launch`] so early events are not lost.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] when the event channel cannot be created. That does not happen
    /// for a non-zero capacity.
    pub fn new(config: SidecarConfig) -> Result<std::sync::Arc<Self>> {
        let (events, keepalive) = broadcast::channel(EVENT_QUEUE_CAPACITY);
        let (shutdown, _) = watch::channel(false);
        let (phase, _) = watch::channel(RunPhase::Starting);
        let (call_tx, call_rx) = mpsc::unbounded_channel();
        let shared = std::sync::Arc::new(Shared {
            status: Mutex::new(Status {
                listener_up: false,
                session: SESSION_DISCONNECTED.to_owned(),
                last_restart_unix_secs: 0,
                restarts: Vec::new(),
                spawn_count: 0,
                crash_loop: false,
                phase: RunPhase::Starting,
            }),
            events,
            keepalive: Mutex::new(keepalive),
            shutdown,
            phase,
        });
        Ok(std::sync::Arc::new(Self {
            shared,
            calls: call_tx,
            inbound: Mutex::new(Some(call_rx)),
            config,
            next_id: AtomicU64::new(1),
            task: Mutex::new(None),
        }))
    }

    /// A receiver for sidecar events. Create it before [`Sidecar::launch`].
    pub fn subscribe(&self) -> broadcast::Receiver<SidecarEvent> {
        self.shared.events.subscribe()
    }

    /// Watches [`RunPhase`]. The current value is available immediately.
    pub fn watch_phase(&self) -> watch::Receiver<RunPhase> {
        self.shared.phase.subscribe()
    }

    /// Starts the supervisor task and the first child.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] when launch is called twice.
    pub fn launch(self: &std::sync::Arc<Self>) -> Result<()> {
        let inbound = self
            .inbound
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        let Some(inbound) = inbound else {
            return Err(Error::Spawn);
        };
        let shared = self.shared.clone();
        let config = self.config.clone();
        let handle = tokio::spawn(async move {
            supervise(config, shared, inbound).await;
        });
        *self.task.lock().unwrap_or_else(|err| err.into_inner()) = Some(handle);
        Ok(())
    }

    /// Current listener, session, and restart counters.
    pub fn status(&self) -> SidecarStatus {
        let status = lock_status(&self.shared);
        SidecarStatus {
            listener_up: status.listener_up,
            session: status.session.clone(),
            last_restart_unix_secs: status.last_restart_unix_secs,
            restarts_24h: restarts_within(&status.restarts, RESTART_WINDOW),
            spawn_count: status.spawn_count,
            crash_loop: status.crash_loop,
            phase: status.phase,
        }
    }

    /// Health of the `zalo-bridge` module.
    pub fn health(&self) -> hub_core::ModuleHealth {
        let status = self.status();
        hub_core::ModuleHealth {
            module: HEALTH_MODULE.to_owned(),
            state: module_state(&status),
            detail: format_detail(&status),
            updated_at_unix_secs: unix_now(),
        }
    }

    /// Sends one request and waits for the response with the same `id`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] when `request_timeout` elapses, [`Error::CrashLoop`] after the
    /// cap, and [`Error::Rpc`] when the sidecar returns `ok: false`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        match self.status().phase {
            RunPhase::CrashLoop => return Err(Error::CrashLoop),
            RunPhase::Stopped => return Err(Error::Stopped),
            RunPhase::Starting | RunPhase::Running | RunPhase::BackingOff => {}
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.calls
            .send(Call {
                id,
                method: method.to_owned(),
                params,
                reply: tx,
            })
            .map_err(|_| Error::Stopped)?;
        match tokio::time::timeout(self.config.request_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_closed)) => Err(Error::Exited),
            Err(_elapsed) => Err(Error::Timeout),
        }
    }

    /// Closes the child's stdin and waits for the supervisor to stop.
    pub async fn shutdown(&self) {
        let _ = self.shared.shutdown.send(true);
        let handle = self
            .task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        let _ = self.shared.shutdown.send(true);
        if let Some(handle) = self
            .task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

fn module_state(status: &SidecarStatus) -> hub_core::ModuleState {
    if !matches!(status.phase, RunPhase::Starting) && status.spawn_count == 0 && !status.crash_loop
    {
        return hub_core::ModuleState::Unknown;
    }
    if status.spawn_count == 0 && status.phase == RunPhase::Starting {
        return hub_core::ModuleState::Unknown;
    }
    if status.crash_loop || status.session == "expired" {
        return hub_core::ModuleState::Down;
    }
    if status.listener_up && status.session == "valid" {
        return hub_core::ModuleState::Ok;
    }
    if status.spawn_count == 0 {
        return hub_core::ModuleState::Unknown;
    }
    hub_core::ModuleState::Degraded
}

fn format_detail(status: &SidecarStatus) -> String {
    let listener = if status.listener_up { "up" } else { "down" };
    let mut detail = format!(
        "listener={listener} session={} last_restart={} restarts_24h={}",
        status.session, status.last_restart_unix_secs, status.restarts_24h
    );
    if status.crash_loop {
        detail.push_str(" crash_loop=1");
    }
    detail
}

async fn supervise(
    config: SidecarConfig,
    shared: std::sync::Arc<Shared>,
    mut calls: mpsc::UnboundedReceiver<Call>,
) {
    if let Ok(rx) = shared.keepalive.lock() {
        let _ = rx.len();
    }
    let mut shutdown = shared.shutdown.subscribe();
    let mut backoff = config.restart_initial;
    let mut streak = 0u32;
    loop {
        if *shutdown.borrow() {
            set_phase(&shared, RunPhase::Stopped);
            return;
        }
        if streak >= config.crash_loop_cap {
            enter_crash_loop(&shared, &mut calls, &mut shutdown).await;
            return;
        }
        let _ = cleanup_media_dir(&media_dir(&config.data_dir));
        let mut child = match spawn_child(&config) {
            Ok(child) => child,
            Err(err) => {
                tracing::warn!(error = %err, "sidecar spawn failed");
                streak += 1;
                set_phase(&shared, RunPhase::BackingOff);
                if !wait_backoff(&mut shutdown, backoff).await {
                    set_phase(&shared, RunPhase::Stopped);
                    return;
                }
                backoff = next_backoff(backoff, config.restart_max);
                continue;
            }
        };
        note_spawn(&shared);
        let Some(stdout) = child.stdout.take() else {
            streak += 1;
            continue;
        };
        let Some(stderr) = child.stderr.take() else {
            streak += 1;
            continue;
        };
        let Some(mut stdin) = child.stdin.take() else {
            streak += 1;
            continue;
        };
        let (line_tx, mut line_rx) = mpsc::channel(32);
        let stdout_task = tokio::spawn(read_stdout(stdout, line_tx.clone()));
        let stderr_task = tokio::spawn(read_stderr(stderr, line_tx));
        let mut inflight: HashMap<u64, oneshot::Sender<Result<Value>>> = HashMap::new();
        let mut saw_frame = false;
        let mut reaped = false;
        let mut stop = false;
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        drop(stdin);
                        stop = true;
                        break;
                    }
                }
                line = line_rx.recv() => {
                    match line {
                        Some(PipeLine::Stdout(text)) => {
                            if text.is_empty() {
                                continue;
                            }
                            if !saw_frame {
                                saw_frame = true;
                                streak = 0;
                                backoff = config.restart_initial;
                            }
                            handle_stdout(&shared, &mut inflight, &text);
                        }
                        Some(PipeLine::Invalid) => {
                            // SECURITY: the line can carry message text, so the warning omits it.
                            tracing::warn!("sidecar stdout line ignored");
                        }
                        Some(PipeLine::Stderr(_text)) => {
                            // SECURITY: stderr is a log, never a frame, and it can echo secrets.
                            tracing::warn!("sidecar stderr");
                        }
                        Some(PipeLine::StdoutClosed) | None => break,
                    }
                }
                call = calls.recv() => {
                    match call {
                        Some(call) => {
                            if write_request(&mut stdin, &call).await.is_err() {
                                let _ = call.reply.send(Err(Error::Exited));
                                break;
                            }
                            inflight.insert(call.id, call.reply);
                        }
                        None => {
                            stop = true;
                            break;
                        }
                    }
                }
                _status = child.wait() => {
                    reaped = true;
                    break;
                }
            }
        }
        stdout_task.abort();
        stderr_task.abort();
        for (_, tx) in inflight.drain() {
            let _ = tx.send(Err(Error::Exited));
        }
        set_listener_down(&shared);
        if !reaped {
            let _ = child.wait().await;
        }
        if stop || *shutdown.borrow() {
            set_phase(&shared, RunPhase::Stopped);
            return;
        }
        streak += 1;
        if streak >= config.crash_loop_cap {
            continue;
        }
        set_phase(&shared, RunPhase::BackingOff);
        if !wait_backoff(&mut shutdown, backoff).await {
            set_phase(&shared, RunPhase::Stopped);
            return;
        }
        backoff = next_backoff(backoff, config.restart_max);
    }
}

enum PipeLine {
    Stdout(String),
    Invalid,
    Stderr(String),
    StdoutClosed,
}

async fn read_stdout(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<PipeLine>) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_capped(&mut reader).await {
            Ok(Some(Ok(line))) => {
                if tx.send(PipeLine::Stdout(line)).await.is_err() {
                    return;
                }
            }
            Ok(Some(Err(()))) => {
                if tx.send(PipeLine::Invalid).await.is_err() {
                    return;
                }
            }
            _ => {
                let _ = tx.send(PipeLine::StdoutClosed).await;
                return;
            }
        }
    }
}

async fn read_stderr(stderr: tokio::process::ChildStderr, tx: mpsc::Sender<PipeLine>) {
    let mut reader = BufReader::new(stderr);
    loop {
        match read_capped(&mut reader).await {
            Ok(Some(Ok(line))) => {
                if line.is_empty() {
                    continue;
                }
                if tx.send(PipeLine::Stderr(line)).await.is_err() {
                    return;
                }
            }
            Ok(Some(Err(()))) => continue,
            _ => return,
        }
    }
}

async fn read_capped(
    reader: &mut BufReader<impl tokio::io::AsyncRead + Unpin>,
) -> std::io::Result<Option<std::result::Result<String, ()>>> {
    let mut buf = Vec::new();
    let n = reader.read_until(b'\n', &mut buf).await?;
    if n == 0 && buf.is_empty() {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    if buf.len() > MAX_STDOUT_LINE_BYTES {
        return Ok(Some(Err(())));
    }
    match String::from_utf8(buf) {
        Ok(text) => Ok(Some(Ok(text))),
        Err(_) => Ok(Some(Err(()))),
    }
}

fn handle_stdout(
    shared: &Shared,
    inflight: &mut HashMap<u64, oneshot::Sender<Result<Value>>>,
    line: &str,
) {
    match parse_stdout(line) {
        StdoutFrame::Response { id, result } => {
            if let Some(tx) = inflight.remove(&id) {
                let _ = tx.send(Ok(result));
            }
        }
        StdoutFrame::Failure {
            id,
            code,
            retry_after_secs,
        } => {
            if let Some(tx) = inflight.remove(&id) {
                let _ = tx.send(Err(Error::Rpc {
                    code,
                    retry_after_secs,
                }));
            }
        }
        StdoutFrame::Event { name, data } => {
            apply_event(shared, &name, &data);
            let _ = shared.events.send(SidecarEvent { name, data });
        }
        StdoutFrame::Ignore => {
            tracing::warn!("sidecar stdout line ignored");
        }
    }
}

fn apply_event(shared: &Shared, name: &str, data: &Value) {
    let mut status = lock_status(shared);
    if name == "session" {
        if let Some(state) = string_field(data, "state") {
            status.session = state;
        }
    } else if name == "listener" {
        status.listener_up = string_field(data, "state").as_deref() == Some(LISTENER_UP);
    }
}

async fn enter_crash_loop(
    shared: &Shared,
    calls: &mut mpsc::UnboundedReceiver<Call>,
    shutdown: &mut watch::Receiver<bool>,
) {
    {
        let mut status = lock_status(shared);
        status.crash_loop = true;
        status.listener_up = false;
        status.phase = RunPhase::CrashLoop;
    }
    let _ = shared.phase.send(RunPhase::CrashLoop);
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                set_phase(shared, RunPhase::Stopped);
                return;
            }
            call = calls.recv() => {
                match call {
                    Some(call) => { let _ = call.reply.send(Err(Error::CrashLoop)); }
                    None => return,
                }
            }
        }
    }
}

fn spawn_child(config: &SidecarConfig) -> Result<Child> {
    let mut cmd = Command::new(&config.program);
    cmd.args(&config.args)
        .env("ZALO_CREDENTIALS_PATH", &config.credentials_path)
        .env("ZALO_DATA_DIR", &config.data_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn().map_err(Error::Io)
}

async fn write_request(stdin: &mut ChildStdin, call: &Call) -> std::io::Result<()> {
    let body = serde_json::json!({
        "id": call.id,
        "method": call.method,
        "params": call.params,
    });
    let mut bytes = serde_json::to_vec(&body)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await?;
    Ok(())
}

fn note_spawn(shared: &Shared) {
    {
        let mut status = lock_status(shared);
        status.spawn_count += 1;
        status.phase = RunPhase::Running;
        if status.spawn_count > 1 {
            status.last_restart_unix_secs = unix_now();
            status.restarts.push(tokio::time::Instant::now());
        }
    }
    let _ = shared.phase.send(RunPhase::Running);
}

fn set_listener_down(shared: &Shared) {
    lock_status(shared).listener_up = false;
}

fn set_phase(shared: &Shared, phase: RunPhase) {
    lock_status(shared).phase = phase;
    let _ = shared.phase.send(phase);
}

async fn wait_backoff(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    if *shutdown.borrow() {
        return false;
    }
    tokio::select! {
        changed = shutdown.changed() => changed.is_ok() && !*shutdown.borrow(),
        _ = tokio::time::sleep(delay) => true,
    }
}

fn next_backoff(current: Duration, max: Duration) -> Duration {
    current.saturating_mul(2).min(max)
}

fn media_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("media")
}

fn restarts_within(restarts: &[tokio::time::Instant], window: Duration) -> u64 {
    let now = tokio::time::Instant::now();
    restarts
        .iter()
        .filter(|started| now.saturating_duration_since(**started) <= window)
        .count() as u64
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lock_status(shared: &Shared) -> MutexGuard<'_, Status> {
    shared.status.lock().unwrap_or_else(|err| err.into_inner())
}
