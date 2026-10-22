//! `captain hub`: the Zalo bridge process.
//!
//! It does not hold the Captain session lock and it does not use the owner's Telegram account.
//! The bot token is a Keychain item under [`captain_vault::HUB_KEYCHAIN_SERVICE`].

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use captain_vault::{HUB_BOT_TOKEN_ACCOUNT, KeychainStore, Profile, Secret, SecretStore};
use clap::Subcommand;
use hub_core::{ModuleHealth, ModuleState};
use hub_store::{EventStream, Hub, SharedHub};
use hub_tgbot::{BotClient, BotConfig, BotToken, PreflightReport, PreflightVerdict};
use hub_web::limits::{MODULE_BRIDGE, MODULE_CAPTAIN_DAEMON, MODULE_ZALO_BRIDGE};
use hub_web::{
    Analytics, AnalyticsQuery, ConversationPage, ConversationQuery, DaysQuery, EventPage,
    HistoryRequest, HistoryResponse, HubApi, ModuleEvent, ModuleList, Overview, OverviewQuery,
    ReloginAck, RetryAck, TelegramPage, ZaloPage,
};
use hub_zalo::{SidecarConfig, ZaloAdapter};
use serde::Serialize;
use serde_json::{Value, json};

#[path = "hub_import.rs"]
mod hub_import;

use crate::hub_config::HubConfig;
use crate::hub_run::{self, RunOptions, default_api_base, lock_hub};
use crate::paths::Paths;
use crate::prompt;

const QR_FILE: &str = "zalo-qr.png";
const QR_DARK: u8 = 128;
#[cfg_attr(not(test), allow(dead_code))]
const MODULE_ORDER: [&str; 3] = [MODULE_CAPTAIN_DAEMON, MODULE_ZALO_BRIDGE, MODULE_BRIDGE];

/// Bridge commands. The daemon is a different process.
#[derive(Subcommand)]
pub enum HubCommand {
    /// Run the sidecar, the bot, and the bridge until Ctrl-C.
    Run {
        /// Write logs to daily files in this directory.
        #[arg(long)]
        log_dir: Option<PathBuf>,
    },
    /// Print module health from the store.
    Status,
    /// Store the bot token in the Keychain.
    #[command(subcommand)]
    Auth(HubAuthCommand),
    /// Zalo session used by the bridge.
    #[command(subcommand)]
    Zalo(HubZaloCommand),
    /// Check that the forum group and the bot rights are usable.
    Preflight,
    /// Copy topic mappings from an old chat-bridges database.
    ///
    /// The default is a dry run that prints counts. `--apply` writes them.
    ImportTopics {
        /// Old `chat-bridges.sqlite3` file. Opened read-only.
        #[arg(long)]
        from: PathBuf,
        /// Write `topic_map`, conversation titles, and user names.
        #[arg(long)]
        apply: bool,
    },
}

/// Bot token commands.
#[derive(Subcommand)]
pub enum HubAuthCommand {
    /// Read a token from a hidden prompt, or from stdin when it is not a terminal.
    SetBotToken,
}

/// Zalo session commands.
#[derive(Subcommand)]
pub enum HubZaloCommand {
    /// Show a QR code and wait until the phone scans it.
    Login,
    /// Delete the local Zalo credentials file.
    Logout,
}

/// Runs one hub command. The bot token store is the hub Keychain, not the MTProto one.
///
/// # Errors
///
/// Returns an error a person can act on. The token value is not part of that text.
pub async fn execute(command: HubCommand, paths: &Paths, profile: &Profile) -> Result<()> {
    let store = KeychainStore::hub_bot();
    match command {
        HubCommand::Run { .. } => {
            let token = load_token(&store, profile)?;
            let running = hub_run::start_hub(RunOptions {
                state_dir: paths.dir.clone(),
                token,
                api_base: default_api_base().to_owned(),
                restart_initial: None,
            })
            .await?;
            wait_for_signal().await;
            running.shutdown().await
        }
        HubCommand::Status => emit(&status_report(&store, profile, paths)?),
        HubCommand::Auth(HubAuthCommand::SetBotToken) => {
            let secret = read_token()?;
            store_bot_token(&store, profile, &secret)?;
            emit(&json!({ "bot_token": "stored" }))
        }
        HubCommand::Zalo(HubZaloCommand::Login) => zalo_login(paths).await,
        HubCommand::Zalo(HubZaloCommand::Logout) => zalo_logout(paths),
        HubCommand::Preflight => preflight(&store, profile, paths).await,
        HubCommand::ImportTopics { from, apply } => import_topics(&from, paths, apply),
    }
}

fn import_topics(from: &Path, paths: &Paths, apply: bool) -> Result<()> {
    let hub = if apply {
        let _lock = match lock_hub(&paths.dir) {
            Ok(lock) => lock,
            Err(err) => bail!("{}", hub_import::lock_failure_text(&err)),
        };
        Some(SharedHub::open(&paths.store()).context("cannot open the hub store")?)
    } else {
        None
    };
    let report = hub_import::import_topics(from, hub.as_ref(), apply)?;
    emit(&report)
}

/// Writes the bot token. Tests pass [`captain_vault::MemoryStore`].
///
/// # Errors
///
/// Returns `cannot store the bot token` when the backend refuses the write.
/// The token and the backend's own message are not included.
pub fn store_bot_token(store: &impl SecretStore, profile: &Profile, secret: &Secret) -> Result<()> {
    if secret.expose().is_empty() {
        bail!("bot token is empty");
    }
    store
        .set(&bot_token_entry(profile), secret)
        .map_err(|_| anyhow::anyhow!("cannot store the bot token"))
}

/// JSON for `captain hub status`. Does not create the database.
///
/// # Errors
///
/// Returns an error when the token entry or an existing store cannot be read.
pub fn status_report(store: &impl SecretStore, profile: &Profile, paths: &Paths) -> Result<Value> {
    let token_state = match store
        .get(&bot_token_entry(profile))
        .map_err(|_| anyhow::anyhow!("cannot read the bot token"))?
    {
        Some(secret) if !secret.expose().is_empty() => "stored",
        _ => "missing",
    };
    let modules = if paths.store().is_file() {
        let hub = Hub::open(&paths.store()).context("cannot open the hub store")?;
        hub.reader()
            .modules()
            .context("cannot read module health")?
    } else {
        Vec::new()
    };
    Ok(json!({
        "bot_token": token_state,
        "modules": modules,
    }))
}

fn bot_token_entry(profile: &Profile) -> String {
    format!("{}/{}", profile.as_str(), HUB_BOT_TOKEN_ACCOUNT)
}

fn load_token(store: &impl SecretStore, profile: &Profile) -> Result<Secret> {
    match store
        .get(&bot_token_entry(profile))
        .map_err(|_| anyhow::anyhow!("cannot read the bot token"))?
    {
        Some(secret) if !secret.expose().is_empty() => Ok(secret),
        _ => bail!("bot token is missing"),
    }
}

fn read_token() -> Result<Secret> {
    let secret = if std::io::stdin().is_terminal() {
        prompt::hidden("Bot token: ").context("cannot read the bot token")?
    } else {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .context("cannot read the bot token")?;
        let token = line.trim();
        if token.is_empty() {
            bail!("bot token is empty");
        }
        Secret::new(token.to_owned())
    };
    if secret.expose().is_empty() {
        bail!("bot token is empty");
    }
    Ok(secret)
}

async fn preflight(store: &impl SecretStore, profile: &Profile, paths: &Paths) -> Result<()> {
    let token = load_token(store, profile)?;
    let config = HubConfig::load(&paths.dir)?;
    let bot = bot_client(&token, default_api_base(), config.group_id)?;
    let report = bot
        .preflight()
        .await
        .map_err(|_| anyhow::anyhow!("preflight failed"))?;
    emit(&preflight_json(&report))
}

/// One line a person can read in the forum topic.
pub fn format_preflight(report: &PreflightReport) -> String {
    format!(
        "verdict {} supergroup {} forum {} manage_topics {} send {} delete {} react {}",
        verdict_name(&report.verdict),
        report.is_supergroup,
        report.is_forum,
        report.can_manage_topics,
        report.can_send_messages,
        report.can_delete_messages,
        report.can_react
    )
}

fn preflight_json(report: &PreflightReport) -> Value {
    json!({
        "verdict": verdict_name(&report.verdict),
        "bot_id": report.bot_id,
        "bot_username": report.bot_username,
        "is_supergroup": report.is_supergroup,
        "is_forum": report.is_forum,
        "can_manage_topics": report.can_manage_topics,
        "can_send_messages": report.can_send_messages,
        "can_delete_messages": report.can_delete_messages,
        "can_react": report.can_react,
    })
}

fn verdict_name(verdict: &PreflightVerdict) -> &'static str {
    match verdict {
        PreflightVerdict::Ready => "ready",
        PreflightVerdict::NotSupergroup => "not_supergroup",
        PreflightVerdict::ForumDisabled => "forum_disabled",
        PreflightVerdict::MissingRights(_) => "missing_rights",
    }
}

fn bot_client(token: &Secret, api_base: &str, group_id: i64) -> Result<BotClient> {
    BotClient::new(BotConfig {
        token: BotToken::new(token.expose().to_owned()),
        api_base_url: api_base.to_owned(),
        group_id,
    })
    .map_err(|_| anyhow::anyhow!("cannot build the bot client"))
}

async fn zalo_login(paths: &Paths) -> Result<()> {
    let _lock = lock_hub(&paths.dir)?;
    let config = HubConfig::load(&paths.dir)?;
    let data_dir = paths.dir.join("zalo");
    let credentials = data_dir.join("credentials.json");
    if !credentials.is_file() {
        if let Some(parent) = credentials.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        captain_vault::write_private(&credentials, b"{}")?;
    }
    let sidecar_config = SidecarConfig::new(
        std::ffi::OsString::from(&config.sidecar_program),
        config
            .sidecar_args
            .iter()
            .map(std::ffi::OsString::from)
            .collect(),
        credentials,
        data_dir,
    );
    let (adapter, admin) = ZaloAdapter::start(sidecar_config)
        .map_err(|_| anyhow::anyhow!("cannot start the zalo sidecar"))?;
    let sidecar = std::sync::Arc::clone(adapter.sidecar());
    let outcome = tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => {
            let _ = admin.cancel_qr().await;
            Err(anyhow::anyhow!("login cancelled"))
        }
        result = drive_qr(&admin, &paths.dir) => result,
    };
    record_qr_event(&paths.dir);
    sidecar.shutdown().await;
    outcome.and_then(|value| emit(&value))
}

async fn drive_qr(admin: &hub_zalo::ZaloAdmin, state_dir: &Path) -> Result<Value> {
    let first = admin
        .start_qr()
        .await
        .map_err(|_| anyhow::anyhow!("qr login failed"))?;
    show_qr(state_dir, &first)?;
    loop {
        let state = admin
            .next_qr_state()
            .await
            .map_err(|_| anyhow::anyhow!("qr login failed"))?;
        if let Some(png) = state.png_base64.as_deref() {
            show_qr(state_dir, png)?;
        }
        match state.state.as_str() {
            "done" => {
                return Ok(json!({
                    "state": "done",
                    "display_name": state.display_name,
                }));
            }
            "expired" => bail!("zalo qr expired"),
            _ => {}
        }
    }
}

fn show_qr(state_dir: &Path, png_base64: &str) -> Result<()> {
    let bytes = decode_base64(png_base64)?;
    let path = write_qr_png(state_dir, &bytes)?;
    let drawing = render_qr_blocks(&bytes)?;
    let mut err = std::io::stderr().lock();
    writeln!(err, "{drawing}")?;
    writeln!(err, "{}", path.display())?;
    Ok(())
}

fn record_qr_event(state_dir: &Path) {
    let Ok(hub) = SharedHub::open(&state_dir.join("inbox.sqlite")) else {
        return;
    };
    let event = hub_store::NewLogEvent {
        at_unix_secs: unix_now(),
        module: MODULE_ZALO_BRIDGE.to_owned(),
        level: hub_store::LogLevel::Info,
        kind: "qr_relogin".into(),
        detail: Some("qr login".into()),
    };
    let _ = hub.lock().record_module_event(&event);
}

fn zalo_logout(paths: &Paths) -> Result<()> {
    let _lock = lock_hub(&paths.dir)?;
    let path = paths.dir.join("zalo").join("credentials.json");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("cannot delete {}", path.display()));
        }
    }
    emit(&json!({ "logged_out": true }))
}

/// Decodes a standard base64 string. Whitespace is ignored.
///
/// # Errors
///
/// Returns an error when the input is not valid base64.
pub fn decode_base64(input: &str) -> Result<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        bail!("qr png is not valid base64");
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|byte| **byte == b'=').count();
        if pad > 2 || chunk[..4 - pad].iter().any(|byte| value(*byte).is_none()) {
            bail!("qr png is not valid base64");
        }
        let a = value(chunk[0]).unwrap_or(0);
        let b = value(chunk[1]).unwrap_or(0);
        let c = if pad < 2 {
            value(chunk[2]).unwrap_or(0)
        } else {
            0
        };
        let d = if pad < 1 {
            value(chunk[3]).unwrap_or(0)
        } else {
            0
        };
        let triple =
            (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

/// Writes the QR PNG with mode 0600 and returns its path.
///
/// # Errors
///
/// Returns an error when the file cannot be written.
pub fn write_qr_png(state_dir: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let path = state_dir.join(QR_FILE);
    captain_vault::write_private(&path, bytes)?;
    Ok(path)
}

/// Draws a QR PNG as half-block rows.
///
/// # Errors
///
/// Returns an error when the bytes are not an 8-bit PNG this function can draw.
pub fn render_qr_blocks(bytes: &[u8]) -> Result<String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|_| anyhow::anyhow!("qr png cannot be read"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|_| anyhow::anyhow!("qr png cannot be read"))?;
    if reader.info().bit_depth != png::BitDepth::Eight {
        bail!("qr png bit depth is unsupported");
    }
    let channels = match reader.info().color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => bail!("qr png color is unsupported"),
    };
    let width = frame.width;
    let height = frame.height;
    let pixels = &buffer[..frame.buffer_size()];
    let mut lines = Vec::new();
    let mut y = 0;
    while y < height {
        let mut line = String::new();
        for x in 0..width {
            let top = dark(pixels, width, channels, x, y);
            let bottom = if y + 1 < height {
                dark(pixels, width, channels, x, y + 1)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        lines.push(line);
        y += 2;
    }
    Ok(lines.join("\n"))
}

fn dark(pixels: &[u8], width: u32, channels: usize, x: u32, y: u32) -> bool {
    let index = (y as usize * width as usize + x as usize) * channels;
    pixels.get(index).copied().unwrap_or(255) < QR_DARK
}

/// Dashboard read of `module_health` and `module_events`. Other pages are not wired here.
///
/// The hub binary does not call this. Tests do, and a dashboard adapter can.
#[cfg_attr(not(test), allow(dead_code))]
pub struct StoreHubApi {
    hub: SharedHub,
}

#[cfg_attr(not(test), allow(dead_code))]
impl StoreHubApi {
    /// Reads `hub`.
    pub fn new(hub: SharedHub) -> Self {
        Self { hub }
    }
}

impl HubApi for StoreHubApi {
    async fn overview(&self, _query: OverviewQuery) -> hub_web::Result<Overview> {
        Err(unsupported())
    }

    async fn conversations(&self, _query: ConversationQuery) -> hub_web::Result<ConversationPage> {
        Err(unsupported())
    }

    async fn analytics(&self, _query: AnalyticsQuery) -> hub_web::Result<Analytics> {
        Err(unsupported())
    }

    async fn telegram(&self, _query: DaysQuery) -> hub_web::Result<TelegramPage> {
        Err(unsupported())
    }

    async fn history(&self, _body: HistoryRequest) -> hub_web::Result<HistoryResponse> {
        Err(unsupported())
    }

    async fn zalo(&self, _query: DaysQuery) -> hub_web::Result<ZaloPage> {
        Err(unsupported())
    }

    async fn modules(&self) -> hub_web::Result<ModuleList> {
        let hub = self.hub.clone();
        let rows = tokio::task::spawn_blocking(move || hub.lock().reader().modules())
            .await
            .map_err(|_| hub_web::Error::Failed("store task stopped".into()))?
            .map_err(|err| hub_web::Error::Failed(err.to_string()))?;
        Ok(ModuleList {
            modules: order_modules(&rows),
        })
    }

    async fn events(&self, limit: u32) -> hub_web::Result<EventPage> {
        let hub = self.hub.clone();
        let rows = tokio::task::spawn_blocking(move || {
            hub.lock()
                .reader()
                .recent_events(EventStream::Module, limit)
        })
        .await
        .map_err(|_| hub_web::Error::Failed("store task stopped".into()))?
        .map_err(|err| hub_web::Error::Failed(err.to_string()))?;
        Ok(EventPage {
            events: rows
                .into_iter()
                .map(|row| ModuleEvent {
                    at_unix_secs: row.at_unix_secs,
                    module: row.module,
                    level: row.level.as_str().to_owned(),
                    kind: row.kind,
                    detail: row.detail.unwrap_or_default(),
                })
                .collect(),
        })
    }

    async fn zalo_relogin(&self) -> hub_web::Result<ReloginAck> {
        Err(unsupported())
    }

    async fn zalo_retry(&self, _id: i64) -> hub_web::Result<RetryAck> {
        Err(unsupported())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn unsupported() -> hub_web::Error {
    hub_web::Error::NotImplemented("not wired on the hub process".into())
}

#[cfg_attr(not(test), allow(dead_code))]
fn order_modules(rows: &[ModuleHealth]) -> Vec<ModuleHealth> {
    MODULE_ORDER
        .into_iter()
        .map(|name| {
            rows.iter()
                .find(|row| row.module == name)
                .cloned()
                .unwrap_or(ModuleHealth {
                    module: name.to_owned(),
                    state: ModuleState::Unknown,
                    detail: String::new(),
                    updated_at_unix_secs: 0,
                })
        })
        .collect()
}

async fn wait_for_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => {
                    let _ = ctrl_c.await;
                    return;
                }
            };
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

fn emit(value: &impl Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

/// Test helper so a run can use a short restart delay without editing `hub.toml`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn run_options(
    state_dir: PathBuf,
    token: Secret,
    api_base: String,
    restart_initial: Option<Duration>,
) -> RunOptions {
    RunOptions {
        state_dir,
        token,
        api_base,
        restart_initial,
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
