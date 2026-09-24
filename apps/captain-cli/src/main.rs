//! `captain`: command-line front end for the protocol layer.
//!
//! Commands print JSON on stdout, one object per line, so they can be piped into `jq` or another
//! program. Prompts, logs, and errors go to stderr. Secrets are only ever read from the
//! environment or a hidden prompt, never from command arguments, which end up in shell history.

mod prompt;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use captain_core::{
    ChatId, ChatRef, Content, MediaItem, MediaKind, MessageId, OutgoingMessage, PhotoQuality,
    Protocol, SendOptions,
};
use captain_format::Syntax;
use captain_protocol::Connection;
use captain_vault::{ApiCredentials, KeychainStore, Profile, Secret, Vault};
use clap::{Args, Parser, Subcommand};
use serde_json::json;

const PROFILE_ENV: &str = "CAPTAIN_PROFILE";
const API_ID_ENV: &str = "TELEGRAM_API_ID";
const API_HASH_ENV: &str = "TELEGRAM_API_HASH";
const LOG_ENV: &str = "RUST_LOG";
const DEFAULT_LOG_FILTER: &str = "warn";

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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage API credentials and the login session.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Print every incoming update as a JSON line until Ctrl-C.
    Listen {
        /// Also deliver updates that arrived while offline.
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
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env(LOG_ENV)
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_LOG_FILTER)),
        )
        .init();

    let cli = Cli::parse();
    let profile = Profile::new(&cli.profile)?;
    let vault = Vault::open(Arc::new(KeychainStore::new()), profile)?;

    match cli.command {
        Command::Auth(AuthCommand::Init) => auth_init(&vault),
        Command::Auth(AuthCommand::Login) => {
            let mut conn = Connection::open(vault)?;
            let me = conn.login(&mut prompt::Terminal).await;
            finish(conn, me.map(|me| json!({ "logged_in": true, "me": me }))).await
        }
        Command::Auth(AuthCommand::Status) => auth_status(vault).await,
        Command::Auth(AuthCommand::Logout) => {
            Connection::open(vault)?.logout().await?;
            emit(&json!({ "logged_in": false }))
        }
        Command::Listen { catch_up } => listen(vault, catch_up).await,
        Command::Send(args) => {
            let message = outgoing(args)?;
            let mut conn = Connection::open(vault)?;
            let sent = match conn.me().await {
                Ok(_) => conn.send(message).await,
                Err(e) => Err(e),
            };
            finish(conn, sent.map(|sent| json!({ "sent": sent }))).await
        }
        Command::Download { chat, message, out } => {
            let mut conn = Connection::open(vault)?;
            let bytes = match conn.me().await {
                Ok(_) => conn.download_media(ChatId(chat), MessageId(message)).await,
                Err(e) => Err(e),
            };
            let result = match bytes {
                Ok(Some(bytes)) => std::fs::write(&out, &bytes)
                    .with_context(|| format!("cannot write {}", out.display()))
                    .map(|()| json!({ "path": out, "size_bytes": bytes.len() })),
                Ok(None) => Err(anyhow::anyhow!(
                    "message {message} in chat {chat} has no media"
                )),
                Err(e) => Err(e.into()),
            };
            conn.close().await?;
            emit(&result?)
        }
    }
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

async fn auth_status(vault: Vault) -> Result<()> {
    let profile = vault.profile().as_str().to_owned();
    if vault.api_credentials()?.is_none() {
        return emit(&json!({ "profile": profile, "api_credentials": false, "logged_in": false }));
    }
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

async fn listen(vault: Vault, catch_up: bool) -> Result<()> {
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
    finish(conn, result.map(|()| serde_json::Value::Null)).await
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
async fn finish(
    conn: Connection,
    value: captain_protocol::Result<serde_json::Value>,
) -> Result<()> {
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
