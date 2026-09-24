//! Periodic work: run heartbeat, ticket resolver, retention and daily size samples.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use captain_inbox::ResolveFacts;
use captain_store::{MetricEvent, MetricKind};

use crate::app::{App, now_unix};

const HEARTBEAT: Duration = Duration::from_secs(60);
const RESOLVE_EVERY: Duration = Duration::from_secs(600);
const DAILY: Duration = Duration::from_secs(86_400);
const CATCH_UP_SETTLE: Duration = Duration::from_secs(120); // catch-up is done by then

/// Runs the jobs until cancelled. Each failure is logged and retried on the next period.
pub async fn run(app: Arc<App>, run_id: i64) {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut resolve = tokio::time::interval(RESOLVE_EVERY);
    let mut daily = tokio::time::interval(DAILY);
    let catch_up = tokio::time::sleep(CATCH_UP_SETTLE);
    tokio::pin!(catch_up);
    let mut catch_up_done = false;
    loop {
        let result = tokio::select! {
            _ = heartbeat.tick() => {
                let now = now_unix();
                app.db.run(move |s| s.heartbeat(run_id, now)).await
            }
            _ = resolve.tick() => resolve_due(&app).await,
            _ = daily.tick() => housekeeping(&app).await,
            () = &mut catch_up, if !catch_up_done => {
                catch_up_done = true;
                record_catch_up(&app, run_id).await
            }
        };
        if let Err(e) = result {
            tracing::error!(error = %e, "periodic job failed");
        }
    }
}

/// Labels every queued or digest ticket whose resolve window has passed.
pub async fn resolve_due(app: &App) -> Result<()> {
    let window = app.config().policy.resolve_window_secs;
    let now = now_unix();
    let resolved = app
        .db
        .run(move |s| {
            let pending = s.pending_resolution(now - window)?;
            for p in &pending {
                let until = p.anchor_date + window;
                let reply_at = s.first_outgoing_on_key(p.key, p.anchor_date, until)?;
                let read_at =
                    s.first_read_covering(p.key.chat, p.key.topic, p.anchor, p.anchor_date, until)?;
                let facts = ResolveFacts {
                    replied_on_key: reply_at.is_some(),
                    replied_in_dm: s.wrote_to_any(&p.senders, p.anchor_date, until)?,
                    read_past_anchor: read_at.is_some(),
                };
                s.resolve_ticket(p.id, facts.label(), read_at, reply_at, now)?;
            }
            Ok(pending.len())
        })
        .await?;
    if resolved > 0 {
        tracing::info!(resolved, "tickets labelled");
    }
    Ok(())
}

async fn housekeeping(app: &App) -> Result<()> {
    let policy = app.config().policy;
    let now = now_unix();
    let deleted = app
        .db
        .run(move |s| {
            let deleted = s.apply_retention(
                now,
                policy.message_retention_days,
                policy.metric_retention_days,
            )?;
            s.record(&MetricEvent {
                a: Some(s.size_bytes()?),
                b: Some(s.message_count()?),
                ..MetricEvent::new(MetricKind::StoreSize, now)
            })?;
            Ok(deleted)
        })
        .await?;
    tracing::info!(deleted, "retention applied");
    Ok(())
}

async fn record_catch_up(app: &App, run_id: i64) -> Result<()> {
    let Some(live) = &app.live else {
        return Ok(());
    };
    let delivered = i64::try_from(live.caught_up.load(Ordering::Relaxed)).unwrap_or(i64::MAX);
    let started = app.started_at;
    app.db
        .run(move |s| {
            let offline = s.previous_run_end(run_id)?.map(|end| started - end);
            s.record(&MetricEvent {
                a: Some(delivered),
                b: offline,
                tag: Some("ok".into()),
                ..MetricEvent::new(MetricKind::CatchUp, started)
            })
        })
        .await
}
