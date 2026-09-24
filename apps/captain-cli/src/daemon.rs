//! `captain daemon`: the one process that owns the session.
//!
//! The main task owns the [`Connection`] and only reads updates from it, because the update
//! stream loses updates when a read is cancelled midway. Everything else holds a
//! [`captain_protocol::Handle`]: the pipeline, the socket, the web UI and the periodic jobs.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use captain_core::Protocol;
use captain_protocol::Connection;
use captain_store::Store;
use captain_vault::Vault;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc};

use crate::app::{App, Config, Db, Live, now_unix};
use crate::paths::Paths;
use crate::pipeline::{Pipeline, PipelineInput};
use crate::telemetry::record_error;
use crate::{ipc_server, jobs, web};

const EVENT_BUFFER: usize = 256; // updates a slow `listen` client may fall behind by
const RETRY_AFTER: Duration = Duration::from_secs(5);
const STOP_SIGNAL: &str = "signal";
const STOP_STREAM_ENDED: &str = "stream_ended";

/// Runs until SIGINT or SIGTERM. Returns an error when the update stream ends by itself, so
/// launchd restarts the daemon, and exits cleanly when the session is not logged in.
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
    let me = match conn.me().await {
        Ok(me) => me,
        Err(e @ captain_protocol::Error::NotAuthorized(_)) => {
            // A clean exit, so launchd does not restart into a login loop. The owner has to
            // run `captain auth login` first.
            tracing::error!(error = %e, "not logged in; the daemon exits until you log in");
            return stop(conn, &db, run_id, "not_authorized", Ok(())).await;
        }
        Err(e) => return stop(conn, &db, run_id, "start_failed", Err(e.into())).await,
    };
    if let Err(e) = conn.listen(true).await {
        return stop(conn, &db, run_id, "listen_failed", Err(e.into())).await;
    }
    // Before the first read, so the catch-up that read triggers can reach every supergroup.
    // Updates that arrive meanwhile wait in grammers' queue.
    match conn.cache_peers().await {
        Ok(chats) => tracing::info!(chats, "chat list cached for catch-up"),
        Err(e) => {
            tracing::warn!(error = %e, "could not cache the chat list; supergroup catch-up may skip gaps");
            record_error(&db, &e, now_unix()).await;
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
    tracing::info!(
        chats = app.config().scope.entries().len(),
        paused,
        "daemon started"
    );

    let reason = read_updates(&app, &mut conn, &pipeline_tx).await?;
    tasks.shutdown().await;
    let _ = std::fs::remove_file(app.paths.socket());
    let result = if reason == STOP_STREAM_ENDED {
        Err(anyhow::anyhow!("the Telegram update stream ended"))
    } else {
        Ok(())
    };
    stop(conn, &db, run_id, reason, result).await
}

/// Reads updates until a stop signal or the end of the stream, and says which.
async fn read_updates(
    app: &App,
    conn: &mut Connection,
    pipeline: &mpsc::UnboundedSender<PipelineInput>,
) -> Result<&'static str> {
    let mut term = signal(SignalKind::terminate())?;
    let Some(live) = &app.live else {
        return Ok(STOP_SIGNAL);
    };
    loop {
        // Dropping `next_event` mid-read loses the updates it was fetching; that only happens
        // on shutdown, and the next start catches up from the last saved position.
        let next = tokio::select! {
            next = conn.next_event(), if !live.unauthorized.load(Ordering::Relaxed) => next,
            _ = tokio::signal::ctrl_c() => return Ok(STOP_SIGNAL),
            _ = term.recv() => return Ok(STOP_SIGNAL),
        };
        match next {
            Ok(Some(event)) => {
                live.last_update_at
                    .store(event.received_at_unix, Ordering::Relaxed);
                // No receivers just means no `listen` client is attached.
                let _ = live.events.send(event.clone());
                if pipeline
                    .send(PipelineInput::Event(Box::new(event)))
                    .is_err()
                {
                    tracing::error!("pipeline stopped; updates are no longer processed");
                }
            }
            Ok(None) => return Ok(STOP_STREAM_ENDED),
            Err(captain_protocol::Error::NotAuthorized(profile)) => {
                // The session was revoked. Keep serving the UI so the owner sees why.
                tracing::error!(profile, "session is no longer authorized; stopped reading");
                live.unauthorized.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::warn!(error = %e, "reading updates failed; retrying");
                record_error(&app.db, &e, now_unix()).await;
                tokio::time::sleep(RETRY_AFTER).await;
            }
        }
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
