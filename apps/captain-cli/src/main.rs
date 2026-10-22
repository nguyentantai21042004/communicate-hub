//! `captain`: command-line front end for the protocol layer and the inbox daemon.
//!
//! Commands print JSON on stdout, one object per line, so they can be piped into `jq` or another
//! program. Prompts, logs, and errors go to stderr. Secrets are only ever read from the
//! environment or a hidden prompt, never from command arguments, which end up in shell history.
//!
//! Only one process may use a session at a time. When `captain daemon` runs, other commands
//! talk to it over its socket; otherwise they take the session lock and connect themselves.

mod app;
mod daemon;
mod dashboard;
mod demo;
mod hub;
mod hub_config;
mod hub_map;
mod hub_run;
mod ipc_server;
mod jobs;
mod paths;
mod pipeline;
mod prompt;
mod report_text;
mod telemetry;
mod web;

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, bail};
use captain_core::{
    ChatId, ChatRef, Content, MediaItem, MediaKind, MessageId, OutgoingMessage, PhotoQuality,
    Protocol, SendOptions,
};
use captain_format::Syntax;
use captain_ipc::{Client, Request};
use captain_protocol::Connection;
use captain_store::Store;
use captain_vault::{ApiCredentials, KeychainStore, Profile, Secret, Vault};
use clap::{Args, Parser, Subcommand};
use hub_store::Hub;
use serde_json::{Value, json};
use tracing_appender::rolling::Rotation;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

use crate::app::{App, Config, Db, now_unix};
use crate::paths::Paths;

const PROFILE_ENV: &str = "CAPTAIN_PROFILE";
const API_ID_ENV: &str = "TELEGRAM_API_ID";
const API_HASH_ENV: &str = "TELEGRAM_API_HASH";
const LOG_ENV: &str = "RUST_LOG";
const DEFAULT_LOG_FILTER: &str = "warn";
const DAEMON_LOG_FILTER: &str = "info";
const LOG_FILES_KEPT: usize = 14; // daily files, so two weeks of daemon logs
const LOG_SUFFIX: &str = "log";
const SECS_PER_DAY: i64 = 86_400;
const DEFAULT_REPORT_DAYS: i64 = 14;
const DEFAULT_CHATS: usize = 50;
const DEFAULT_HISTORY_SCAN: usize = 3000; // 30 getHistory pages of 100
const OPEN_COMMAND: &str = "open"; // macOS: opens a URL in the default browser

#[derive(Parser)]
#[command(
    name = "captain",
    version,
    about = "Receive and send Telegram messages as your own account"
)]
struct Cli {
    /// Profile name; each profile is a separate account and Keychain namespace.
    #[arg(long, global = true, env = PROFILE_ENV, default_value = "default")]
    profile: String,
    /// Use this directory instead of the profile's state directory.
    #[arg(long, global = true, hide = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage API credentials and the login session.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Run the inbox: read updates, create tickets, serve the socket and the web UI.
    Daemon {
        /// Write logs to daily files in this directory, keeping two weeks, instead of stderr.
        #[arg(long)]
        log_dir: Option<PathBuf>,
    },
    /// Show the daemon's state, or the store's when it is not running.
    Status,
    /// Stop creating tickets. Messages in scope are still stored.
    Pause,
    /// Start creating tickets again.
    Resume,
    /// Print every incoming update as a JSON line until Ctrl-C.
    Listen {
        /// Also deliver updates that arrived while offline. Ignored when the daemon runs,
        /// which always catches up.
        #[arg(long)]
        catch_up: bool,
    },
    /// Send a message, photo, file, or album as the logged-in account.
    Send(SendArgs),
    /// Save the media attached to a message.
    Download {
        /// Marked chat id, as printed by `listen`.
        #[arg(long, allow_hyphen_values = true)]
        chat: i64,
        /// Message id within the chat.
        #[arg(long)]
        message: i32,
        /// Where to write the file.
        #[arg(long)]
        out: PathBuf,
    },
    /// Recent chats with their ids, for writing scope.toml.
    Chats {
        /// How many.
        #[arg(long, default_value_t = DEFAULT_CHATS)]
        limit: usize,
    },
    /// Search a chat's older messages on Telegram with a regex. Needs the daemon. Matches
    /// print oldest first with a t.me link; nothing is stored or marked read.
    History {
        /// Marked chat id, as printed by `chats`.
        #[arg(long, allow_hyphen_values = true)]
        chat: i64,
        /// Regular expression for text and captions, case-insensitive. `(?-i)` turns that off.
        #[arg(long)]
        grep: String,
        /// Start below this message id. Pass the last `oldest_scanned` to page further back.
        #[arg(long)]
        before: Option<i32>,
        /// Messages to read, newest first, 100 per request.
        #[arg(long, default_value_t = DEFAULT_HISTORY_SCAN)]
        scan: usize,
    },
    /// List tickets, newest first.
    Tickets(TicketsArgs),
    /// Show, label or create one ticket.
    #[command(subcommand)]
    Ticket(TicketCommand),
    /// The phase 1 metrics report.
    Report {
        /// Days to cover.
        #[arg(long, default_value_t = DEFAULT_REPORT_DAYS)]
        days: i64,
        /// Print a readable report instead of JSON.
        #[arg(long)]
        text: bool,
    },
    /// Check or reload scope.toml and policy.toml.
    #[command(subcommand)]
    Scope(ScopeCommand),
    /// Print the web UI address, signed in, or serve the UI without Telegram.
    Ui {
        #[command(subcommand)]
        command: Option<UiCommand>,
        /// Open it in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// Run the Zalo bridge. This process does not hold the Captain session.
    #[command(subcommand)]
    Hub(hub::HubCommand),
    /// Development helpers.
    #[command(subcommand, hide = true)]
    Dev(DevCommand),
}

#[derive(Subcommand)]
enum AuthCommand {
    // Help text lives in attributes: clap prints doc comments verbatim, so rustdoc markup
    // such as backticks around env var names would show up in `--help`.
    #[command(
        about = "Store the API id and hash from https://my.telegram.org/apps in the Keychain",
        long_about = "Store the API id and hash from https://my.telegram.org/apps in the Keychain.\n\n\
            Reads TELEGRAM_API_ID and TELEGRAM_API_HASH if set, otherwise prompts."
    )]
    Init,
    /// Log in with phone number, code, and two-step password if the account has one.
    Login,
    /// Show whether credentials are stored and which account is logged in.
    Status,
    /// End the session on Telegram and delete the local auth key.
    Logout,
}

#[derive(Args)]
struct TicketsArgs {
    /// Only this state: skipped, digest, queued or resolved.
    #[arg(long)]
    state: Option<String>,
    // In `help` rather than a doc comment: clap prints doc comments verbatim, backticks too.
    #[arg(
        long,
        help = "Only this trigger: dm_all, mention, reply_to_me, keyword or manual"
    )]
    trigger: Option<String>,
    /// Only tickets opened in the last this many days.
    #[arg(long)]
    days: Option<i64>,
    /// Only tickets you have not labelled.
    #[arg(long)]
    unlabelled: bool,
    /// At most this many.
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Subcommand)]
enum TicketCommand {
    /// A ticket with its history and message text.
    Show {
        /// Ticket id.
        id: i64,
    },
    /// Say whether this ticket was worth a draft: want, noise, skip, or clear.
    Label {
        /// Ticket id.
        id: i64,
        /// want, noise, skip, or clear.
        label: String,
    },
    /// Create a ticket for one message by hand. Needs the daemon.
    New {
        /// Marked chat id.
        #[arg(long, allow_hyphen_values = true)]
        chat: i64,
        /// Message id within the chat.
        #[arg(long)]
        message: i32,
        /// Instruction for the draft.
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum ScopeCommand {
    /// Parse the files and print what they watch.
    Check,
    /// Make the running daemon reread them.
    Reload,
}

#[derive(Subcommand)]
enum UiCommand {
    /// Serve the UI from the store. Takes the session lock unless --attach.
    Serve {
        /// Read-only next to a running daemon. No session lock, no Telegram.
        #[arg(long)]
        attach: bool,
    },
}

#[derive(Subcommand)]
enum DevCommand {
    /// Fill an empty store with two weeks of made-up tickets and metrics.
    SeedDemo,
}

#[derive(Args)]
struct SendArgs {
    /// Destination: `me`, `@username`, or a marked chat id seen in `listen`.
    #[arg(long, allow_hyphen_values = true)]
    to: String,
    /// Message text, or the caption when media is attached.
    #[arg(long, default_value = "")]
    text: String,
    /// Photo to send; repeat for an album of up to 10.
    #[arg(long)]
    photo: Vec<PathBuf>,
    /// File to send as a document; repeat for an album of files.
    #[arg(long)]
    file: Vec<PathBuf>,
    /// Send photos as original files instead of compressed HD photos.
    #[arg(long)]
    original: bool,
    /// Blur photos until the recipient taps them.
    #[arg(long)]
    spoiler: bool,
    /// Show the caption above the media.
    #[arg(long)]
    caption_above: bool,
    /// Reply to this message id.
    #[arg(long)]
    reply_to: Option<i32>,
    /// Send without a notification sound.
    #[arg(long)]
    silent: bool,
    /// Do not generate a link preview.
    #[arg(long)]
    no_preview: bool,
    /// Allow the longer Premium caption limit.
    #[arg(long)]
    premium_caption: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let default_filter = if matches!(
        cli.command,
        Command::Daemon { .. } | Command::Hub(hub::HubCommand::Run { .. })
    ) {
        DAEMON_LOG_FILTER
    } else {
        DEFAULT_LOG_FILTER
    };
    let log_dir = match &cli.command {
        Command::Daemon { log_dir } | Command::Hub(hub::HubCommand::Run { log_dir }) => {
            log_dir.as_deref()
        }
        _ => None,
    };
    if let Some(dir) = log_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create the log directory {}", dir.display()))?;
    }
    let writer = match log_dir {
        Some(dir) => BoxMakeWriter::new(
            tracing_appender::rolling::Builder::new()
                .rotation(Rotation::DAILY)
                .filename_prefix(&cli.profile)
                .filename_suffix(LOG_SUFFIX)
                .max_log_files(LOG_FILES_KEPT)
                .build(dir)
                .with_context(|| format!("cannot write logs in {}", dir.display()))?,
        ),
        None => BoxMakeWriter::new(std::io::stderr),
    };
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(log_dir.is_none())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env(LOG_ENV)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .init();

    let profile = Profile::new(&cli.profile)?;
    let keychain = Arc::new(KeychainStore::new());
    let vault = match &cli.state_dir {
        Some(dir) => Vault::with_state_dir(keychain, profile, dir.clone()),
        None => Vault::open(keychain, profile)?,
    };
    let paths = Paths::new(vault.state_dir());

    match cli.command {
        Command::Auth(AuthCommand::Init) => auth_init(&vault),
        Command::Auth(AuthCommand::Login) => {
            let _lock = paths.lock_session()?;
            let mut conn = Connection::open(vault)?;
            let me = conn.login(&mut prompt::Terminal).await;
            finish(conn, me.map(|me| json!({ "logged_in": true, "me": me }))).await
        }
        Command::Auth(AuthCommand::Status) => auth_status(vault, &paths).await,
        Command::Auth(AuthCommand::Logout) => {
            let _lock = paths.lock_session()?;
            Connection::open(vault)?.logout().await?;
            emit(&json!({ "logged_in": false }))
        }
        Command::Daemon { .. } => daemon::run(vault, paths, web::Settings::from_env()?).await,
        Command::Status => emit(&call(&vault, &paths, Request::Status).await?),
        Command::Pause => emit(&call(&vault, &paths, Request::Pause).await?),
        Command::Resume => emit(&call(&vault, &paths, Request::Resume).await?),
        Command::Listen { catch_up } => listen(vault, &paths, catch_up).await,
        Command::Send(args) => {
            let message = outgoing(args)?;
            if let Some(mut daemon) = daemon(&paths).await? {
                return emit(&daemon.call(Request::Send { message }).await?);
            }
            let (_lock, conn) = connect(vault, &paths).await?;
            let sent = conn.send(message).await;
            finish(conn, sent.map(|sent| json!({ "sent": sent }))).await
        }
        Command::Download { chat, message, out } => {
            download(vault, &paths, ChatId(chat), MessageId(message), out).await
        }
        Command::Chats { limit } => {
            if let Some(mut daemon) = daemon(&paths).await? {
                return emit(&daemon.call(Request::Chats { limit }).await?);
            }
            let (_lock, conn) = connect(vault, &paths).await?;
            let dialogs = match conn.handle() {
                Ok(handle) => handle.dialogs(limit).await,
                Err(e) => Err(e),
            };
            finish(conn, dialogs.map(|d| json!(d))).await
        }
        Command::History {
            chat,
            grep,
            before,
            scan,
        } => {
            let Some(mut daemon) = daemon(&paths).await? else {
                bail!("history search needs the daemon; start it with `captain daemon`");
            };
            let request = Request::History {
                chat: ChatId(chat),
                pattern: grep,
                before: before.map(MessageId),
                scan_max: scan,
            };
            emit(&daemon.call(request).await?)
        }
        Command::Tickets(args) => {
            let request = Request::Tickets {
                state: args.state,
                trigger: args.trigger,
                within_secs: args.days.map(|d| d * SECS_PER_DAY),
                unlabelled: args.unlabelled,
                limit: args.limit,
            };
            emit(&call(&vault, &paths, request).await?)
        }
        Command::Ticket(TicketCommand::Show { id }) => {
            emit(&call(&vault, &paths, Request::Ticket { id }).await?)
        }
        Command::Ticket(TicketCommand::Label { id, label }) => {
            let label = (label != "clear").then_some(label);
            emit(&call(&vault, &paths, Request::Label { id, label }).await?)
        }
        Command::Ticket(TicketCommand::New {
            chat,
            message,
            note,
        }) => {
            let Some(mut daemon) = daemon(&paths).await? else {
                bail!("manual tickets need the daemon; start it with `captain daemon`");
            };
            let request = Request::NewTicket {
                chat: ChatId(chat),
                message: MessageId(message),
                note,
            };
            emit(&daemon.call(request).await?)
        }
        Command::Report { days, text } => {
            let request = Request::Report {
                within_secs: days.max(1) * SECS_PER_DAY,
            };
            let report = call(&vault, &paths, request).await?;
            if text {
                let mut out = std::io::stdout().lock();
                out.write_all(report_text::render(&report).as_bytes())?;
                Ok(())
            } else {
                emit(&report)
            }
        }
        Command::Scope(ScopeCommand::Check) => {
            let config = Config::load(&paths)?;
            emit(&json!({
                "path": paths.scope(),
                "missing": config.scope_missing,
                "entries": config.scope.entries(),
                "policy": *config.policy,
            }))
        }
        Command::Scope(ScopeCommand::Reload) => {
            let Some(mut daemon) = daemon(&paths).await? else {
                bail!("the daemon is not running; it reads the files when it starts");
            };
            emit(&daemon.call(Request::ScopeReload).await?)
        }
        Command::Ui {
            command: Some(UiCommand::Serve { attach }),
            ..
        } => ui_serve(&vault, &paths, attach).await,
        Command::Ui {
            command: None,
            open,
        } => ui(&paths, open).await,
        Command::Hub(command) => hub::execute(command, &paths, vault.profile()).await,
        Command::Dev(DevCommand::SeedDemo) => {
            if cli.state_dir.is_none() {
                bail!("seed-demo writes made-up data; give it an empty --state-dir");
            }
            let store = Store::open(&paths.store())?;
            let mut hub = Hub::open(&paths.store())?;
            let seeded = demo::seed(&store, &mut hub, now_unix())?;
            if !paths.scope().exists() {
                std::fs::write(paths.scope(), demo::scope_document())?;
            }
            emit(&json!({ "store": paths.store(), "scope": paths.scope(), "tickets": seeded }))
        }
    }
}

/// The daemon's socket, or `None` when no daemon listens.
async fn daemon(paths: &Paths) -> Result<Option<Client>> {
    match Client::connect(&paths.socket()).await {
        Ok(client) => Ok(Some(client)),
        Err(captain_ipc::Error::NotRunning(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Answers a store-only request through the daemon, or from the store when it is stopped.
async fn call(vault: &Vault, paths: &Paths, request: Request) -> Result<Value> {
    if let Some(mut daemon) = daemon(paths).await? {
        return Ok(daemon.call(request).await?);
    }
    let app = offline_app(vault, paths)?;
    ipc_server::handle(&app, request).await
}

/// The app without Telegram: store, scope and policy only.
fn offline_app(vault: &Vault, paths: &Paths) -> Result<Arc<App>> {
    store_app(vault, paths, Store::open(&paths.store())?)
}

/// The app for `--attach`: existing store, read-only, no Telegram and no lock.
fn attach_app(vault: &Vault, paths: &Paths) -> Result<Arc<App>> {
    let store = Store::open_read_only(&paths.store())?;
    let _ = Hub::open_read_only(&paths.store())?;
    store_app(vault, paths, store)
}

fn store_app(vault: &Vault, paths: &Paths, store: Store) -> Result<Arc<App>> {
    let paused = store.paused()?;
    Ok(Arc::new(App {
        db: Db::new(store),
        paths: paths.clone(),
        profile: vault.profile().as_str().to_owned(),
        config: RwLock::new(Config::load(paths)?),
        paused: AtomicBool::new(paused),
        started_at: now_unix(),
        live: None,
        ui_url: RwLock::new(None),
    }))
}

/// Takes the session lock and connects as the logged-in account.
async fn connect(vault: Vault, paths: &Paths) -> Result<(paths::SessionLock, Connection)> {
    let lock = paths.lock_session()?;
    let mut conn = Connection::open(vault)?;
    if let Err(e) = conn.me().await {
        conn.close().await?;
        return Err(e.into());
    }
    Ok((lock, conn))
}

fn auth_init(vault: &Vault) -> Result<()> {
    let api_id = match std::env::var(API_ID_ENV) {
        Ok(v) => v,
        Err(_) => prompt::line("API id: ")?,
    };
    let api_id: i32 = api_id
        .trim()
        .parse()
        .with_context(|| format!("{API_ID_ENV} must be a number"))?;
    let api_hash = match std::env::var(API_HASH_ENV) {
        Ok(v) => Secret::new(v),
        Err(_) => prompt::hidden("API hash (hidden): ")?,
    };
    if api_hash.expose().trim().is_empty() {
        bail!("API hash is empty");
    }
    vault.set_api_credentials(&ApiCredentials { api_id, api_hash })?;
    emit(&json!({ "profile": vault.profile().as_str(), "api_credentials": "stored in keychain" }))
}

async fn auth_status(vault: Vault, paths: &Paths) -> Result<()> {
    let profile = vault.profile().as_str().to_owned();
    if vault.api_credentials()?.is_none() {
        return emit(&json!({ "profile": profile, "api_credentials": false, "logged_in": false }));
    }
    if let Some(mut daemon) = daemon(paths).await? {
        let status = daemon.call(Request::Status).await?;
        let logged_in = status["connection"] == "connected";
        return emit(&json!({
            "profile": profile,
            "api_credentials": true,
            "logged_in": logged_in,
            "me": status["me"],
            "via": "daemon",
        }));
    }
    let _lock = paths.lock_session()?;
    let mut conn = Connection::open(vault)?;
    let status = match conn.me().await {
        Ok(me) => {
            Ok(json!({ "profile": profile, "api_credentials": true, "logged_in": true, "me": me }))
        }
        Err(captain_protocol::Error::NotAuthorized(_)) => {
            Ok(json!({ "profile": profile, "api_credentials": true, "logged_in": false }))
        }
        Err(e) => Err(e),
    };
    finish(conn, status).await
}

async fn listen(vault: Vault, paths: &Paths, catch_up: bool) -> Result<()> {
    if let Some(mut daemon) = daemon(paths).await? {
        daemon.call(Request::Subscribe).await?;
        tracing::info!("listening through the daemon; press Ctrl-C to stop");
        loop {
            tokio::select! {
                line = daemon.next_line() => match line? {
                    Some(line) => emit(line.get("event").unwrap_or(&line))?,
                    None => bail!("the daemon closed the connection"),
                },
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
    let _lock = paths.lock_session()?;
    let mut conn = Connection::open(vault)?;
    if let Err(e) = conn.listen(catch_up).await {
        return finish(conn, Err(e)).await;
    }
    tracing::info!("listening; press Ctrl-C to stop");
    let result = loop {
        tokio::select! {
            event = conn.next_event() => match event {
                Ok(Some(event)) => emit(&event)?,
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            },
            _ = tokio::signal::ctrl_c() => break Ok(()),
        }
    };
    finish(conn, result.map(|()| Value::Null)).await
}

async fn download(
    vault: Vault,
    paths: &Paths,
    chat: ChatId,
    message: MessageId,
    out: PathBuf,
) -> Result<()> {
    if let Some(mut daemon) = daemon(paths).await? {
        // The daemon may run as another working directory; hand it an absolute path.
        let out = std::path::absolute(&out)?;
        return emit(
            &daemon
                .call(Request::Download { chat, message, out })
                .await?,
        );
    }
    let (_lock, conn) = connect(vault, paths).await?;
    let result = match conn.download_media(chat, message).await {
        Ok(Some(bytes)) => std::fs::write(&out, &bytes)
            .with_context(|| format!("cannot write {}", out.display()))
            .map(|()| json!({ "path": out, "size_bytes": bytes.len() })),
        Ok(None) => Err(anyhow::anyhow!(
            "message {} in chat {} has no media",
            message.0,
            chat.0
        )),
        Err(e) => Err(e.into()),
    };
    conn.close().await?;
    emit(&result?)
}

async fn ui(paths: &Paths, open: bool) -> Result<()> {
    let Some(mut daemon) = daemon(paths).await? else {
        bail!("the daemon is not running; start it, or use `captain ui serve` to browse the store");
    };
    let status = daemon.call(Request::Status).await?;
    if status["ui_url"].is_null() {
        bail!("the daemon is not serving the UI");
    }
    let url = web::Settings::from_env()?.login_url(&paths.ui_token()?);
    open_url(&url, open)
}

async fn ui_serve(vault: &Vault, paths: &Paths, attach: bool) -> Result<()> {
    // Holding the lock keeps a daemon from starting against the same port and store.
    // `--attach` skips it so this process can sit next to the daemon and `hub run`.
    let _lock = if attach {
        None
    } else {
        Some(
            paths
                .lock_session()
                .context("the daemon is running; use `captain ui` to open its UI")?,
        )
    };
    let app = if attach {
        attach_app(vault, paths)?
    } else {
        offline_app(vault, paths)?
    };
    let settings = if attach {
        web::Settings::from_env_or(web::DEFAULT_ATTACH_PORT)?
    } else {
        web::Settings::from_env()?
    };
    let listener = web::bind(&app, &settings).await?;
    let token = paths.ui_token()?;
    let url = settings.login_url(&token);
    if attach {
        emit(&json!({
            "url": url,
            "urls": settings.attach_login_urls(&token),
        }))?;
        tracing::warn!("serving the store read-only; press Ctrl-C to stop");
        tokio::select! {
            () = web::serve_attach(app, listener, token) => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    } else {
        open_url(&url, false)?;
        tracing::warn!("serving the store without Telegram; press Ctrl-C to stop");
        tokio::select! {
            () = web::serve(app, listener, token) => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    Ok(())
}

/// Prints `{"url": ...}`, and opens it when asked.
fn open_url(url: &str, open: bool) -> Result<()> {
    emit(&json!({ "url": url }))?;
    if open {
        std::process::Command::new(OPEN_COMMAND)
            .arg(url)
            .status()
            .with_context(|| format!("cannot run `{OPEN_COMMAND}`"))?;
    }
    Ok(())
}

/// Builds the message from flags. Photos and files cannot be mixed in one album, which
/// validation reports before any upload.
fn outgoing(args: SendArgs) -> Result<OutgoingMessage> {
    let to: ChatRef = args.to.parse()?;
    let text = captain_format::parse(&args.text, Syntax::Plain);
    let quality = if args.original {
        PhotoQuality::Original
    } else {
        PhotoQuality::Hd
    };
    let photos = args.photo.into_iter().map(|path| MediaItem {
        kind: MediaKind::Photo {
            quality,
            spoiler: args.spoiler,
        },
        ..MediaItem::photo(path)
    });
    let items: Vec<MediaItem> = photos
        .chain(args.file.into_iter().map(MediaItem::document))
        .collect();
    let content = if items.is_empty() {
        Content::Text(text)
    } else {
        Content::Media {
            items,
            caption: text,
            caption_above: args.caption_above,
        }
    };
    let options = SendOptions {
        reply_to: args.reply_to.map(MessageId),
        silent: args.silent,
        no_link_preview: args.no_preview,
        premium_caption: args.premium_caption,
    };
    let message = OutgoingMessage {
        to,
        content,
        options,
    };
    message.validate()?;
    Ok(message)
}

/// Closes the connection, then prints `value` unless it is `null`.
async fn finish(conn: Connection, value: captain_protocol::Result<Value>) -> Result<()> {
    let closed = conn.close().await;
    let value = value?;
    closed?;
    if value.is_null() {
        Ok(())
    } else {
        emit(&value)
    }
}

fn emit(value: &impl serde::Serialize) -> Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}
