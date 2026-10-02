//! The daemon side of the local socket.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Arc;

use anyhow::{Context, Result};
use captain_ipc::{Request, Response, decode_request, encode_line};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;

use crate::app::App;

const SOCKET_MODE: u32 = 0o600;

/// Binds the socket. The caller holds the session lock, so a socket file left behind by a
/// crashed daemon can be removed safely.
pub fn bind(app: &App) -> Result<(UnixListener, u32)> {
    let path = app.paths.socket();
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    let owner = std::fs::metadata(&path)?.uid();
    Ok((listener, owner))
}

/// Accepts clients until cancelled.
pub async fn serve(app: Arc<App>, listener: UnixListener, owner: u32) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "socket accept failed");
                continue;
            }
        };
        // SECURITY: the socket is 0600, but check the peer too, so a permissions mistake on
        // the state directory never exposes the account.
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == owner => {}
            Ok(cred) => {
                tracing::warn!(uid = cred.uid(), "rejected socket client from another user");
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot identify socket client");
                continue;
            }
        }
        tokio::spawn(client(Arc::clone(&app), stream));
    }
}

async fn client(app: Arc<App>, stream: UnixStream) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(error = %e, "socket client went away");
                return;
            }
        };
        let (id, request) = match decode_request(&line) {
            Ok(decoded) => decoded,
            Err(e) => {
                let _ = send(&mut write, &Response::err(0, e)).await;
                continue;
            }
        };
        if request == Request::Subscribe {
            stream_events(&app, id, &mut write).await;
            return;
        }
        let response = match handle(&app, request).await {
            Ok(result) => Response::ok(id, result),
            Err(e) => Response::err(id, format!("{e:#}")),
        };
        if send(&mut write, &response).await.is_err() {
            return;
        }
    }
}

async fn send(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    value: &impl serde::Serialize,
) -> Result<()> {
    write.write_all(encode_line(value)?.as_bytes()).await?;
    Ok(())
}

async fn stream_events(app: &App, id: u64, write: &mut tokio::net::unix::OwnedWriteHalf) {
    let Some(live) = &app.live else {
        let _ = send(
            write,
            &Response::err(id, "the daemon has no Telegram connection"),
        )
        .await;
        return;
    };
    let mut events = live.events.subscribe();
    if send(write, &Response::ok(id, Value::Null)).await.is_err() {
        return;
    }
    loop {
        match events.recv().await {
            Ok(event) => {
                if send(write, &json!({ "event": event })).await.is_err() {
                    return;
                }
            }
            Err(RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "a listen client fell behind");
                if send(write, &json!({ "skipped": skipped })).await.is_err() {
                    return;
                }
            }
            Err(RecvError::Closed) => return,
        }
    }
}

/// Answers one request; `captain` also uses this directly when no daemon runs.
pub async fn handle(app: &App, request: Request) -> Result<Value> {
    match request {
        Request::Status => app.status().await,
        Request::Pause => app.set_paused(true).await,
        Request::Resume => app.set_paused(false).await,
        Request::Subscribe => Ok(Value::Null),
        Request::Send { message } => app.send(message).await,
        Request::Download { chat, message, out } => app.download(chat, message, out).await,
        Request::Chats { limit } => app.chats(limit).await,
        Request::History {
            chat,
            pattern,
            before,
            scan_max,
        } => app.history(chat, &pattern, before, scan_max).await,
        Request::Tickets {
            state,
            trigger,
            within_secs,
            unlabelled,
            limit,
        } => {
            app.tickets(
                state.as_deref(),
                trigger.as_deref(),
                within_secs,
                unlabelled,
                limit,
            )
            .await
        }
        Request::Ticket { id } => app.ticket(id, true).await,
        Request::Label { id, label } => app.label(id, label.as_deref()).await,
        Request::NewTicket {
            chat,
            message,
            note,
        } => app.new_ticket(chat, message, note).await,
        Request::Report { within_secs } => app.report(within_secs).await,
        Request::ScopeReload => app.reload(),
    }
}
