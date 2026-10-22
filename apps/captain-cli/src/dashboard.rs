//! Dashboard numbers for the local pages.
//!
//! Hub pages read hub-analytics and hub-store. Telegram connection, session, triggers,
//! scope, and sender names still come from Captain's store and the live daemon. A missing
//! module heartbeat is `down` with the reason "no heartbeat". The request does not fail for that.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Context;
use captain_core::ChatId;
use captain_inbox::report::{self, TicketFact};
use captain_inbox::{Route, Scope, TicketState, TriggerKind, stats, unix_day};
use captain_store::{TicketFilter, TicketRow};
use chrono::TimeZone;
use hub_analytics::Source as MetricSource;
use hub_store::{ConversationKind as HubKind, FailureRow, Network, Reader, TicketSnap};
use hub_web::limits::{
    ATTENTION_LIMIT, CANDIDATE_LIMIT, CONNECTION_OFFLINE, DRAFT_UNAVAILABLE, MODULE_BRIDGE,
    MODULE_CAPTAIN_DAEMON, MODULE_ZALO_BRIDGE, SECS_PER_DAY, SECS_PER_MINUTE, STORAGE_KEYCHAIN,
    STORAGE_NONE, TICKET_LIST_CAP, TOP_CONVERSATIONS,
};
use hub_web::{
    ActiveWatched, Analytics, AnalyticsQuery, AttentionRow, BridgeCard, ConversationKindFilter,
    ConversationPage, ConversationQuery, ConversationRow, DailyMessages, DailyResponse, DaysQuery,
    Error, EventPage, FailedEvent, GroupGap, HeatCell, HistoryMatch, HistoryRequest,
    HistoryResponse, HubApi, KindCount, LabelCount, ModuleEvent, ModuleHealth, ModuleList,
    ModuleState, ModuleSummary, Overview, OverviewKpi, OverviewQuery, Result, ScopeCandidate,
    ScopeFunnel, SendQueue, Source, SourceCount, SourceFilter, TelegramConnection, TelegramPage,
    TelegramSession, TicketFunnel, TopConversation, TopicMapRow, TriggerStat, ZaloPage,
    ZaloSession, mask_external_id,
};
use serde_json::Value;

use crate::app::{self, App, now_unix};

const P50: u8 = 50; // median
const P99: u8 = 99; // update-lag tail
const KIND_DM: &str = "dm";
const KIND_GROUP: &str = "group";
const UNTITLED: &str = "Untitled";
const NO_HEARTBEAT: &str = "no heartbeat";
const STORAGE_SIDECAR: &str = "sidecar"; // Zalo session file kept by the bridge, never the secret
const SESSION_VALID: &str = "valid";
const SESSION_CONNECTING: &str = "connecting";
const SESSION_UNKNOWN: &str = "unknown";
const DAEMON_REVOKED_DETAIL: &str = "session was revoked";
const DAEMON_RECONNECTING_DETAIL: &str = "reconnecting";
const RELOGIN_UNWIRED: &str = "zalo QR login is not wired";
const RETRY_UNWIRED: &str = "zalo retry is not wired";
const DAEMON_DEGRADED_DETAIL: &str = "degraded";
const TOP_SENDERS: usize = 10; // Telegram page ranking
const SECS_PER_HOUR: i64 = 3_600;
const FRIEND_REQUEST_KIND: &str = "friend_request";
const RATE_LIMITED_KIND: &str = "rate_limited";
const RETRY_AFTER_KEY: &str = "retry_after_secs=";
const LABEL_ANSWERED: &str = "answered";
const LABEL_ANSWERED_ELSEWHERE: &str = "answered_elsewhere";
const LABEL_READ_ONLY: &str = "read_only";
const LABEL_UNSEEN: &str = "unseen";
const LABEL_UNLABELLED: &str = "unlabelled";
const MODULE_ORDER: [&str; 3] = [MODULE_CAPTAIN_DAEMON, MODULE_ZALO_BRIDGE, MODULE_BRIDGE];

/// Reads dashboard numbers from the hub store, Captain's store, and the daemon.
#[derive(Clone)]
pub(crate) struct Adapter {
    app: Arc<App>,
}

impl Adapter {
    pub(crate) fn new(app: Arc<App>) -> Self {
        Self { app }
    }
}

struct Loaded {
    input: report::ReportInput,
    open: Vec<TicketRow>,
    now: i64,
}

impl HubApi for Adapter {
    async fn overview(&self, query: OverviewQuery) -> Result<Overview> {
        let app = Arc::clone(&self.app);
        let now = now_unix();
        let offset = local_offset_minutes(now);
        block(move || overview_at(&app, query, now, offset)).await
    }

    async fn conversations(&self, query: ConversationQuery) -> Result<ConversationPage> {
        let app = Arc::clone(&self.app);
        let now = now_unix();
        block(move || conversations_at(&app, &query, now)).await
    }

    async fn analytics(&self, query: AnalyticsQuery) -> Result<Analytics> {
        let app = Arc::clone(&self.app);
        let now = now_unix();
        let offset = local_offset_minutes(now);
        block(move || analytics_at(&app, query, now, offset)).await
    }

    async fn telegram(&self, query: DaysQuery) -> Result<TelegramPage> {
        let loaded = load(&self.app, query.days).await?;
        let scope = self.app.config().scope;
        let (uptime, gaps_unfilled, last_catch_up_unix_secs) = link_stats(&loaded.input);
        let since = loaded
            .now
            .saturating_sub(i64::from(query.days).saturating_mul(SECS_PER_DAY));
        let until = loaded.now;
        let senders = self
            .app
            .db
            .run(move |store| app::sender_counts(store, since, until, TOP_SENDERS))
            .await
            .map_err(|error| fail(&error))?;
        Ok(TelegramPage {
            connection: TelegramConnection {
                uptime,
                gaps_unfilled,
                lag_p50_secs: lag_secs(&loaded.input.update_lags_secs, P50),
                lag_p99_secs: lag_secs(&loaded.input.update_lags_secs, P99),
                last_catch_up_unix_secs,
                reconnects_24h: reconnects(&self.app, loaded.now),
            },
            session: session(&self.app),
            triggers: trigger_stats(&loaded.input.tickets),
            scope: scope_funnel(&scope, &loaded),
            candidates: candidates(&loaded.input.candidates),
            top_senders: senders
                .into_iter()
                .map(|(name, count)| hub_web::SenderCount { name, count })
                .collect(),
        })
    }

    async fn history(&self, body: HistoryRequest) -> Result<HistoryResponse> {
        let value = self
            .app
            .history(ChatId(body.chat), &body.regex, None, body.scan_limit)
            .await
            .map_err(|error| fail(&error))?;
        map_history(&value, body.text)
    }

    async fn zalo(&self, _query: DaysQuery) -> Result<ZaloPage> {
        let app = Arc::clone(&self.app);
        let now = now_unix();
        block(move || zalo_at(&app, now)).await
    }

    async fn modules(&self) -> Result<ModuleList> {
        let app = Arc::clone(&self.app);
        let now = now_unix();
        block(move || {
            let hub = open_hub(&app)?;
            Ok(ModuleList {
                modules: module_strip(&app, &hub.reader(), now)?,
            })
        })
        .await
    }

    async fn events(&self, limit: u32) -> Result<EventPage> {
        let path = self.app.paths.store();
        block(move || events_at(&path, limit)).await
    }

    async fn zalo_relogin(&self) -> Result<hub_web::ReloginAck> {
        Err(Error::NotImplemented(RELOGIN_UNWIRED.to_owned()))
    }

    async fn zalo_retry(&self, _id: i64) -> Result<hub_web::RetryAck> {
        Err(Error::NotImplemented(RETRY_UNWIRED.to_owned()))
    }
}

const STATE_OK: &str = "ok";
const STATE_DEGRADED: &str = "degraded";
const STATE_DOWN: &str = "down";
const STATE_UNKNOWN: &str = "unknown";

async fn block<T: Send + 'static>(
    task: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T> {
    match tokio::task::spawn_blocking(task).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(fail(&error)),
        Err(error) => Err(Error::Failed(format!("store task failed: {error}"))),
    }
}

fn open_hub(app: &App) -> anyhow::Result<hub_store::Hub> {
    hub_store::Hub::open(&app.paths.store()).context("hub store")
}

fn metric_source(source: SourceFilter) -> MetricSource {
    match source {
        SourceFilter::All => MetricSource::All,
        SourceFilter::Telegram => MetricSource::Telegram,
        SourceFilter::Zalo => MetricSource::Zalo,
    }
}

fn one_source(source: Source) -> MetricSource {
    match source {
        Source::Telegram => MetricSource::Telegram,
        Source::Zalo => MetricSource::Zalo,
    }
}

fn web_source(network: Network) -> Option<Source> {
    match network {
        Network::Telegram => Some(Source::Telegram),
        Network::Zalo => Some(Source::Zalo),
        Network::Messenger => None,
    }
}

/// System offset of `now`, in minutes east of UTC. `0` when that instant is skipped.
fn local_offset_minutes(now: i64) -> i32 {
    let minute = i32::try_from(SECS_PER_MINUTE).unwrap_or(1);
    match chrono::Local.timestamp_opt(now, 0) {
        chrono::LocalResult::Single(local) | chrono::LocalResult::Ambiguous(local, _) => {
            local.offset().local_minus_utc() / minute
        }
        chrono::LocalResult::None => 0,
    }
}

fn shifted_day(unix: i64, offset_minutes: i32) -> i64 {
    let shift = i64::from(offset_minutes).saturating_mul(SECS_PER_MINUTE);
    unix.saturating_add(shift).div_euclid(SECS_PER_DAY)
}

fn overview_at(
    app: &App,
    query: OverviewQuery,
    now: i64,
    offset_minutes: i32,
) -> anyhow::Result<Overview> {
    let hub = open_hub(app)?;
    let reader = hub.reader();
    let days = i64::from(query.days);
    let card = hub_analytics::overview(&reader, now, days, metric_source(query.source))
        .context("overview")?;
    let modules = module_strip(app, &reader, now)?;
    let (needs_attention, rows) = attention_rows(&reader, query.source, now)?;
    Ok(Overview {
        kpi: OverviewKpi {
            messages_24h: message_rows(&card.messages_24h, query.source),
            needs_attention,
            response_p50_minutes: card.response_p50_minutes,
            response_sample: card.response_samples,
            conversations: activity(&reader, now, days, query.source)?,
            module_summary: summarize(&modules),
        },
        series: daily_series(&reader, now, days, query.source, offset_minutes)?,
        needs_attention: rows,
        modules,
    })
}

fn message_rows(rows: &[hub_analytics::SourceCount], source: SourceFilter) -> Vec<SourceCount> {
    source
        .sources()
        .iter()
        .copied()
        .map(|net| SourceCount {
            source: net,
            messages: rows
                .iter()
                .find(|row| web_source(row.source) == Some(net))
                .map(|row| row.count)
                .unwrap_or(0),
        })
        .collect()
}

fn activity(
    reader: &Reader<'_>,
    now: i64,
    days: i64,
    source: SourceFilter,
) -> anyhow::Result<Vec<ActiveWatched>> {
    let mut rows = Vec::new();
    for net in source.sources() {
        let card =
            hub_analytics::overview(reader, now, days, one_source(*net)).context("activity")?;
        rows.push(ActiveWatched {
            source: *net,
            active: card.active_conversations,
            watched: card.watched_conversations,
        });
    }
    Ok(rows)
}

fn daily_series(
    reader: &Reader<'_>,
    now: i64,
    days: i64,
    source: SourceFilter,
    offset_minutes: i32,
) -> anyhow::Result<Vec<DailyMessages>> {
    let today = shifted_day(now, offset_minutes);
    let first = today - days + 1;
    let shift = i64::from(offset_minutes).saturating_mul(SECS_PER_MINUTE);
    let start_unix = first.saturating_mul(SECS_PER_DAY).saturating_sub(shift);
    let end_unix = today
        .saturating_add(1)
        .saturating_mul(SECS_PER_DAY)
        .saturating_sub(shift)
        .saturating_sub(1);
    let rows = reader
        .counters_between_days(
            start_unix.div_euclid(SECS_PER_DAY),
            end_unix.div_euclid(SECS_PER_DAY),
        )
        .context("counters")?;
    let nets = source.sources();
    let width = nets.len();
    let span = usize::try_from(days).unwrap_or(0);
    let mut totals = vec![0u64; span.saturating_mul(width)];
    for row in rows {
        let Some(net) = web_source(row.network) else {
            continue;
        };
        let Some(column) = nets.iter().position(|item| *item == net) else {
            continue;
        };
        let bucket = row
            .day
            .saturating_mul(SECS_PER_DAY)
            .saturating_add(i64::from(row.hour).saturating_mul(SECS_PER_HOUR));
        let day = shifted_day(bucket, offset_minutes);
        if day < first || day > today {
            continue;
        }
        let Ok(row_index) = usize::try_from(day - first) else {
            continue;
        };
        let Some(slot) = totals.get_mut(row_index.saturating_mul(width).saturating_add(column))
        else {
            continue;
        };
        *slot = slot.saturating_add(row.count);
    }
    let mut out = Vec::new();
    let mut day = first;
    let mut row_index = 0usize;
    while day <= today {
        for (column, net) in nets.iter().enumerate() {
            let messages = totals
                .get(row_index.saturating_mul(width).saturating_add(column))
                .copied()
                .unwrap_or(0);
            out.push(DailyMessages {
                day,
                source: *net,
                messages,
            });
        }
        day += 1;
        row_index += 1;
    }
    Ok(out)
}

fn attention_rows(
    reader: &Reader<'_>,
    source: SourceFilter,
    now: i64,
) -> anyhow::Result<(u64, Vec<AttentionRow>)> {
    let queued = TicketState::Queued.as_str();
    let mut rows: Vec<TicketSnap> = reader
        .open_tickets()
        .context("open tickets")?
        .into_iter()
        .filter(|ticket| ticket.state == queued)
        .filter(|ticket| web_source(ticket.source).is_some_and(|net| source.includes(net)))
        .collect();
    let count = u64_len(rows.len());
    rows.sort_by_key(|ticket| ticket.opened_at_unix_secs);
    let attention = rows
        .into_iter()
        .take(ATTENTION_LIMIT)
        .filter_map(|ticket| {
            Some(AttentionRow {
                source: web_source(ticket.source)?,
                title: titled(&ticket.chat_label),
                trigger: ticket.trigger,
                age_secs: now.saturating_sub(ticket.opened_at_unix_secs),
            })
        })
        .collect();
    Ok((count, attention))
}

fn analytics_at(
    app: &App,
    query: AnalyticsQuery,
    now: i64,
    offset_minutes: i32,
) -> anyhow::Result<Analytics> {
    let hub = open_hub(app)?;
    let reader = hub.reader();
    let days = i64::from(query.days);
    let metric = metric_source(query.source);
    let heat = hub_analytics::hourly_heatmap(&reader, now, days, metric, offset_minutes)
        .context("heatmap")?;
    let funnel = hub_analytics::ticket_funnel(&reader, now, days, metric).context("funnel")?;
    let ranked =
        hub_analytics::top_conversations(&reader, now, days, u64_len(TOP_CONVERSATIONS), metric)
            .context("top conversations")?;
    let response =
        hub_analytics::response_p50_by_day(&reader, now, days, metric).context("response")?;
    Ok(Analytics {
        heatmap: heat
            .cells
            .into_iter()
            .map(|cell| HeatCell {
                weekday: cell.weekday,
                hour: cell.hour,
                messages: cell.count,
            })
            .collect(),
        funnel: TicketFunnel {
            queued: funnel.queued,
            draft: DRAFT_UNAVAILABLE.to_owned(),
            resolved: label_counts(funnel.resolved_labels),
        },
        top_conversations: ranked
            .into_iter()
            .filter_map(|row| {
                Some(TopConversation {
                    source: web_source(row.network)?,
                    title: display_title(row.network, row.title, &row.external_id),
                    messages: row.messages,
                })
            })
            .collect(),
        response_p50: response
            .into_iter()
            .map(|row| DailyResponse {
                day: row.day,
                p50_minutes: row.p50_minutes,
                sample: row.samples,
                low_sample: row.low_sample,
            })
            .collect(),
    })
}

fn label_counts(labels: hub_analytics::ResolvedLabels) -> Vec<LabelCount> {
    let mut rows = vec![
        (LABEL_ANSWERED, labels.answered),
        (LABEL_ANSWERED_ELSEWHERE, labels.answered_elsewhere),
        (LABEL_READ_ONLY, labels.read_only),
        (LABEL_UNSEEN, labels.unseen),
    ];
    if labels.unlabelled > 0 {
        rows.push((LABEL_UNLABELLED, labels.unlabelled));
    }
    rows.into_iter()
        .map(|(label, count)| LabelCount {
            label: label.to_owned(),
            count,
        })
        .collect()
}

fn conversations_at(
    app: &App,
    query: &ConversationQuery,
    now: i64,
) -> anyhow::Result<ConversationPage> {
    let kind = match query.kind {
        Some(ConversationKindFilter::Channel | ConversationKindFilter::Forum) => {
            return Ok(ConversationPage {
                rows: Vec::new(),
                total: 0,
            });
        }
        Some(ConversationKindFilter::Dm) => Some(HubKind::Dm),
        Some(ConversationKindFilter::Group) => Some(HubKind::Group),
        None => None,
    };
    let hub = open_hub(app)?;
    let page = hub_analytics::conversations_page(
        &hub.reader(),
        now,
        &hub_analytics::ConversationFilter {
            source: metric_source(query.source),
            kind,
            days: query.days.map(i64::from),
            query: query.q.clone(),
        },
        u64::from(query.limit),
        u64::from(query.offset),
    )
    .context("conversations")?;
    let rows = page
        .items
        .into_iter()
        .filter_map(|item| {
            let external_id = if item.network == Network::Zalo {
                mask_external_id(&item.external_id)
            } else {
                item.external_id.clone()
            };
            Some(ConversationRow {
                source: web_source(item.network)?,
                title: display_title(item.network, item.title.clone(), &item.external_id),
                external_id,
                kind: item.kind.as_str().to_owned(),
                messages_7d: item.sparkline,
                last_activity_unix_secs: Some(item.last_activity_unix_secs),
                open_tickets: item.open_tickets,
                tag: item.tags.filter(|tag| !tag.trim().is_empty()),
            })
        })
        .collect();
    Ok(ConversationPage {
        rows,
        total: page.total,
    })
}

fn zalo_at(app: &App, now: i64) -> anyhow::Result<ZaloPage> {
    let hub = open_hub(app)?;
    let reader = hub.reader();
    let stored = reader.modules().context("modules")?;
    let bridge = stored
        .iter()
        .find(|module| module.module == MODULE_ZALO_BRIDGE);
    let (bridge_state, bridge_detail, session) = match bridge {
        Some(module) => (
            module.state,
            module.detail.clone(),
            zalo_session(true, module.state),
        ),
        None => (
            ModuleState::Down,
            NO_HEARTBEAT.to_owned(),
            zalo_session(false, ModuleState::Down),
        ),
    };
    let conversations = reader.conversations().context("conversations")?;
    let mut gaps = Vec::new();
    let mut topics = Vec::new();
    let mut titles = BTreeMap::new();
    for conv in &conversations {
        let title = display_title(conv.network, conv.title.clone(), &conv.external_id);
        titles.insert(conv.id, title.clone());
        if conv.network != Network::Zalo {
            continue;
        }
        if conv.topic_id.is_some() {
            topics.push(TopicMapRow {
                title,
                external_id: mask_external_id(&conv.external_id),
                topic_id: conv.topic_id,
                muted: conv.muted,
            });
        } else if conv.kind == HubKind::Group {
            gaps.push(title);
        }
    }
    let failed = reader.failures(true).context("failures")?;
    let failed_events = failed
        .into_iter()
        .filter(|row| row.network.is_none() || row.network == Some(Network::Zalo))
        .map(|row| failed_event(row, &titles))
        .collect();
    let summary = hub_analytics::zalo_summary(&reader, now, u64_len(gaps.len())).context("zalo")?;
    let path = app.paths.store();
    let since = now.saturating_sub(SECS_PER_DAY);
    let rate_limits_24h = app::count_module_kind(&path, RATE_LIMITED_KIND, Some(since), now)?;
    let pending_friend_requests = app::count_module_kind(&path, FRIEND_REQUEST_KIND, None, now)?;
    let retry_detail = app::latest_module_detail(&path, RATE_LIMITED_KIND, Some(since), now)?;
    Ok(ZaloPage {
        bridge: BridgeCard {
            state: state_name(bridge_state).to_owned(),
            detail: bridge_detail,
        },
        session,
        send_queue: SendQueue {
            waiting: 0,
            rate_limits_24h,
            retry_after_secs: retry_detail.as_deref().and_then(retry_after_secs),
        },
        topics,
        message_kinds_14d: summary
            .kinds
            .into_iter()
            .map(|kind| KindCount {
                kind: kind.kind.as_str().to_owned(),
                count: kind.count,
            })
            .collect(),
        failed_events,
        pending_friend_requests,
        groups_without_topic: GroupGap {
            count: u64_len(gaps.len()),
            names: gaps,
        },
    })
}

fn failed_event(row: FailureRow, titles: &BTreeMap<i64, String>) -> FailedEvent {
    let conversation = row
        .conversation_id
        .and_then(|id| titles.get(&id).cloned())
        .unwrap_or_else(|| UNTITLED.to_owned());
    FailedEvent {
        id: row.id,
        reason: row.reason,
        conversation,
        at_unix_secs: row.created_at_unix_secs,
        retryable: row.retryable,
    }
}

fn retry_after_secs(detail: &str) -> Option<u64> {
    let rest = detail.split(RETRY_AFTER_KEY).nth(1)?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn zalo_session(stored: bool, state: ModuleState) -> ZaloSession {
    ZaloSession {
        account: None,
        storage: if stored {
            STORAGE_SIDECAR
        } else {
            STORAGE_NONE
        }
        .to_owned(),
        state: match state {
            ModuleState::Ok => SESSION_VALID,
            ModuleState::Degraded => SESSION_CONNECTING,
            ModuleState::Down | ModuleState::Unknown => SESSION_UNKNOWN,
        }
        .to_owned(),
    }
}

fn state_name(state: ModuleState) -> &'static str {
    match state {
        ModuleState::Ok => STATE_OK,
        ModuleState::Degraded => STATE_DEGRADED,
        ModuleState::Down => STATE_DOWN,
        ModuleState::Unknown => STATE_UNKNOWN,
    }
}

fn events_at(path: &Path, limit: u32) -> anyhow::Result<EventPage> {
    let _hub = hub_store::Hub::open(path).context("hub store")?;
    let logs = app::module_logs(path, limit)?;
    Ok(EventPage {
        events: logs
            .into_iter()
            .map(|log| ModuleEvent {
                at_unix_secs: log.at_unix_secs,
                module: log.module,
                level: log.level,
                kind: log.kind,
                detail: log.detail,
            })
            .collect(),
    })
}

fn module_strip(app: &App, reader: &Reader<'_>, now: i64) -> anyhow::Result<Vec<ModuleHealth>> {
    let stored = reader.modules().context("modules")?;
    Ok(fill_modules(app, &stored, now))
}

fn fill_modules(app: &App, stored: &[ModuleHealth], now: i64) -> Vec<ModuleHealth> {
    MODULE_ORDER
        .iter()
        .map(|name| {
            if let Some(row) = stored.iter().find(|row| row.module == *name) {
                return row.clone();
            }
            if *name == MODULE_CAPTAIN_DAEMON {
                daemon_health(app, now)
            } else {
                ModuleHealth {
                    module: (*name).to_owned(),
                    state: ModuleState::Down,
                    detail: NO_HEARTBEAT.to_owned(),
                    updated_at_unix_secs: now,
                }
            }
        })
        .collect()
}

fn display_title(network: Network, title: Option<String>, external_id: &str) -> String {
    if let Some(title) = title
        && let Some(trimmed) = non_empty(&title)
    {
        return trimmed.to_owned();
    }
    if network == Network::Zalo {
        mask_external_id(external_id)
    } else if external_id.is_empty() {
        UNTITLED.to_owned()
    } else {
        external_id.to_owned()
    }
}

async fn load(app: &App, days: u32) -> Result<Loaded> {
    let now = now_unix();
    let since = day_bounds(now, days);
    let until = now;
    let mut input = app
        .db
        .run(move |store| store.report_input(since, until))
        .await
        .map_err(|error| fail(&error))?;
    extend_running_run(app, &mut input, until);
    let open = open_tickets(app).await?;
    Ok(Loaded { input, open, now })
}

fn day_bounds(now: i64, days: u32) -> i64 {
    let today = unix_day(now);
    let first = today - i64::from(days) + 1;
    first.saturating_mul(SECS_PER_DAY)
}

/// The stored run ends at its last heartbeat. This process is still the same run.
fn extend_running_run(app: &App, input: &mut report::ReportInput, until: i64) {
    if app.live.is_none() {
        return;
    }
    for run in &mut input.runs {
        if run.started_at == app.started_at {
            run.stopped_at = until;
        }
    }
}

async fn open_tickets(app: &App) -> Result<Vec<TicketRow>> {
    let mut rows = fetch_state(app, TicketState::Queued).await?;
    rows.extend(fetch_state(app, TicketState::Digest).await?);
    Ok(rows)
}

async fn fetch_state(app: &App, state: TicketState) -> Result<Vec<TicketRow>> {
    app.db
        .run(move |store| {
            store.tickets(&TicketFilter {
                state: Some(state),
                limit: Some(TICKET_LIST_CAP),
                ..TicketFilter::default()
            })
        })
        .await
        .map_err(|error| fail(&error))
}

fn trigger_stats(tickets: &[TicketFact]) -> Vec<TriggerStat> {
    const TRIGGERS: [TriggerKind; 4] = [
        TriggerKind::DmAll,
        TriggerKind::Mention,
        TriggerKind::ReplyToMe,
        TriggerKind::Keyword,
    ];
    TRIGGERS
        .into_iter()
        .map(|trigger| one_trigger(tickets, trigger))
        .collect()
}

fn one_trigger(tickets: &[TicketFact], trigger: TriggerKind) -> TriggerStat {
    let rows: Vec<&TicketFact> = tickets
        .iter()
        .filter(|ticket| ticket.trigger == trigger)
        .collect();
    let open = rows
        .iter()
        .filter(|ticket| matches!(ticket.state, TicketState::Queued | TicketState::Digest))
        .count();
    let resolved = rows
        .iter()
        .filter(|ticket| ticket.state == TicketState::Resolved)
        .count();
    let mut drafts = 0;
    let mut digests = 0;
    for ticket in &rows {
        match ticket.route {
            Some(Route::Draft) => drafts += 1,
            Some(Route::Digest) => digests += 1,
            None => {}
        }
    }
    TriggerStat {
        trigger: trigger.as_str().to_owned(),
        count: u64_len(rows.len()),
        open: u64_len(open),
        resolved_ratio: stats::ratio(resolved, rows.len()),
        route: majority_route(drafts, digests),
    }
}

fn majority_route(drafts: usize, digests: usize) -> Option<String> {
    if drafts == 0 && digests == 0 {
        None
    } else if drafts >= digests {
        Some(Route::Draft.as_str().to_owned())
    } else {
        Some(Route::Digest.as_str().to_owned())
    }
}

fn scope_funnel(scope: &Scope, loaded: &Loaded) -> ScopeFunnel {
    let watched_ids = scope_ids(scope);
    ScopeFunnel {
        watched: u64_len(watched_ids.len()),
        with_messages: u64_len(
            loaded
                .input
                .inbound
                .iter()
                .filter(|row| row.count > 0 && watched_ids.contains(&row.chat.0))
                .map(|row| row.chat.0)
                .collect::<BTreeSet<_>>()
                .len(),
        ),
        with_tickets: u64_len(
            loaded
                .input
                .tickets
                .iter()
                .filter(|ticket| watched_ids.contains(&ticket.key.chat.0))
                .map(|ticket| ticket.key.chat.0)
                .collect::<BTreeSet<_>>()
                .len(),
        ),
        with_open_tickets: u64_len(
            loaded
                .open
                .iter()
                .filter(|ticket| watched_ids.contains(&ticket.chat.0))
                .map(|ticket| ticket.chat.0)
                .collect::<BTreeSet<_>>()
                .len(),
        ),
    }
}

fn scope_ids(scope: &Scope) -> BTreeSet<i64> {
    scope.entries().iter().map(|entry| entry.id.0).collect()
}

fn candidates(rows: &[report::Candidate]) -> Vec<ScopeCandidate> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        right
            .would_trigger
            .cmp(&left.would_trigger)
            .then(left.chat.0.cmp(&right.chat.0))
    });
    rows.truncate(CANDIDATE_LIMIT);
    rows.into_iter()
        .map(|row| {
            let title = row
                .title
                .as_deref()
                .and_then(non_empty)
                .unwrap_or(UNTITLED)
                .to_owned();
            let snippet = scope_snippet(row.chat.0, &title);
            ScopeCandidate {
                title,
                would_trigger: u64_len(row.would_trigger),
                messages: u64_len(row.messages),
                snippet,
            }
        })
        .collect()
}

fn scope_snippet(chat: i64, title: &str) -> String {
    let title = title.replace(['"', '\\'], "");
    let dm = chat > 0;
    let kind = if dm { KIND_DM } else { KIND_GROUP };
    let triggers = if dm {
        "\"dm_all\""
    } else {
        "\"mention\", \"reply_to_me\""
    };
    format!(
        "[[chat]]\nid = {chat}\nlabel = \"{title}\"\nkind = \"{kind}\"\ntriggers = [{triggers}]\n"
    )
}

/// Same window arithmetic as the report's uptime and gap metrics. Those helpers are private.
fn link_stats(input: &report::ReportInput) -> (Option<f64>, u64, Option<i64>) {
    let since = input.since;
    let until = input.until;
    let watched_from = input
        .first_run_at
        .map_or(since, |first| first.clamp(since, until));
    let mut spans: Vec<(i64, i64, bool)> = input
        .runs
        .iter()
        .map(|run| {
            (
                run.started_at.max(since),
                run.stopped_at.min(until),
                run.caught_up,
            )
        })
        .filter(|(start, stop, _)| stop > start)
        .collect();
    spans.sort_unstable();
    let mut covered = 0;
    let mut cursor = watched_from;
    for (start, stop, _) in &spans {
        let from = (*start).max(cursor);
        if *stop > from {
            covered += stop - from;
            cursor = *stop;
        }
    }
    let window = (until - watched_from).max(1);
    let uptime = (!spans.is_empty()).then(|| as_f64(covered) / as_f64(window));
    let gaps = spans
        .windows(2)
        .filter(|pair| !pair[1].2)
        .map(|pair| pair[1].0 - pair[0].1)
        .filter(|gap| *gap > 0)
        .count();
    let last_catch_up = input
        .runs
        .iter()
        .filter(|run| run.caught_up)
        .map(|run| run.started_at)
        .max();
    (uptime, u64_len(gaps), last_catch_up)
}

fn lag_secs(values: &[i64], percentile: u8) -> Option<f64> {
    stats::percentile(values, percentile).map(as_f64)
}

fn session(app: &App) -> TelegramSession {
    let (connection, account) = match &app.live {
        None => (CONNECTION_OFFLINE, None),
        Some(live) => (
            live.health
                .connection(live.unauthorized.load(Ordering::Relaxed)),
            live.me.name.clone(),
        ),
    };
    TelegramSession {
        account,
        storage: STORAGE_KEYCHAIN.to_owned(),
        connection: connection.to_owned(),
    }
}

fn reconnects(app: &App, now: i64) -> u64 {
    app.live
        .as_ref()
        .map_or(0, |live| u64_len(live.health.reconnects_24h(now)))
}

fn daemon_health(app: &App, now: i64) -> ModuleHealth {
    let (state, detail) = match &app.live {
        None => (ModuleState::Down, NO_HEARTBEAT.to_owned()),
        Some(live) if live.unauthorized.load(Ordering::Relaxed) => {
            (ModuleState::Down, DAEMON_REVOKED_DETAIL.to_owned())
        }
        Some(live) if live.health.reconnecting.load(Ordering::Relaxed) => {
            (ModuleState::Degraded, DAEMON_RECONNECTING_DETAIL.to_owned())
        }
        Some(live) if live.health.degraded.load(Ordering::Relaxed) => (
            ModuleState::Degraded,
            live.health
                .degraded_reason()
                .unwrap_or_else(|| DAEMON_DEGRADED_DETAIL.to_owned()),
        ),
        Some(_) => (ModuleState::Ok, String::new()),
    };
    ModuleHealth {
        module: MODULE_CAPTAIN_DAEMON.to_owned(),
        state,
        detail,
        updated_at_unix_secs: now,
    }
}

fn summarize(modules: &[ModuleHealth]) -> ModuleSummary {
    let mut summary = ModuleSummary {
        ok: 0,
        degraded: 0,
        down: 0,
        unknown: 0,
    };
    for module in modules {
        match module.state {
            ModuleState::Ok => summary.ok += 1,
            ModuleState::Degraded => summary.degraded += 1,
            ModuleState::Down => summary.down += 1,
            ModuleState::Unknown => summary.unknown += 1,
        }
    }
    summary
}

fn map_history(value: &Value, include_text: bool) -> Result<HistoryResponse> {
    let matches = value
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Failed("history response has no matches".into()))?;
    let mut rows = Vec::with_capacity(matches.len());
    for row in matches {
        rows.push(map_match(row, include_text)?);
    }
    Ok(HistoryResponse {
        chat: required_i64(value, "chat")?,
        scanned: required_u64(value, "scanned")?,
        oldest_scanned: optional_i64(value, "oldest_scanned")?,
        more_before: value
            .get("more_before")
            .and_then(Value::as_bool)
            .ok_or_else(|| Error::Failed("history response has no more_before".into()))?,
        matches: rows,
    })
}

fn map_match(row: &Value, include_text: bool) -> Result<HistoryMatch> {
    Ok(HistoryMatch {
        id: required_i64(row, "id")?,
        date_unix: required_i64(row, "date_unix")?,
        sender: optional_string(row, "sender")?,
        text: if include_text {
            optional_string(row, "text")?
        } else {
            None
        },
        link: optional_string(row, "link")?,
    })
}

fn required_i64(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::Failed(format!("history response has no {key}")))
}

fn optional_i64(value: &Value, key: &str) -> Result<Option<i64>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(inner) => inner
            .as_i64()
            .map(Some)
            .ok_or_else(|| Error::Failed(format!("history response has no {key}"))),
    }
}

fn required_u64(value: &Value, key: &str) -> Result<u64> {
    let number = value
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .and_then(|inner| u64::try_from(inner).ok())
        })
        .ok_or_else(|| Error::Failed(format!("history response has no {key}")))?;
    Ok(number)
}

fn optional_string(value: &Value, key: &str) -> Result<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Error::Failed(format!("history response has no {key}"))),
    }
}

fn titled(label: &str) -> String {
    non_empty(label).unwrap_or(UNTITLED).to_owned()
}

fn non_empty(label: &str) -> Option<&str> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn u64_len(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

fn fail(error: &anyhow::Error) -> Error {
    Error::Failed(format!("{error:#}"))
}

#[expect(clippy::cast_precision_loss, reason = "seconds stay far below 2^52")]
fn as_f64(value: i64) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};

    use captain_inbox::{Policy, Scope};
    use captain_store::Store;
    use hub_store::{
        ContentKind, ConversationId, ConversationKind, CountedMessage, Direction, Hub, Network,
        NewConversation,
    };
    use hub_web::{
        AnalyticsQuery, ConversationKindFilter, ConversationQuery, DaysQuery, HistoryRequest,
        HubApi, ModuleState, OverviewQuery, Source, SourceFilter,
    };

    use super::{analytics_at, overview_at};
    use crate::app::{App, Config, Db, now_unix};
    use crate::demo;
    use crate::paths::Paths;

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "captain-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open_app(dir: &std::path::Path, scope: Scope) -> Arc<App> {
        let paths = Paths::new(dir);
        let store = Store::open(&paths.store()).unwrap();
        Arc::new(App {
            db: Db::new(store),
            paths,
            profile: "test".into(),
            config: RwLock::new(Config {
                scope: Arc::new(scope),
                policy: Arc::new(Policy::default()),
                scope_missing: false,
            }),
            paused: AtomicBool::new(false),
            started_at: 0,
            live: None,
            ui_url: RwLock::new(None),
        })
    }

    fn conv(source: SourceFilter, kind: Option<ConversationKindFilter>) -> ConversationQuery {
        ConversationQuery {
            source,
            kind,
            days: None,
            q: None,
            limit: 200,
            offset: 0,
        }
    }

    fn assert_daily_shape(rows: &[hub_web::DailyMessages], source: Source) -> u64 {
        let points: Vec<_> = rows.iter().filter(|row| row.source == source).collect();
        assert_eq!(points.len(), 14);
        let nonzero = points.iter().filter(|row| row.messages > 0).count();
        assert!(nonzero >= 12, "{source:?} nonzero {nonzero}");
        let sum: u64 = points.iter().map(|row| row.messages).sum();
        let max = points.iter().map(|row| row.messages).max().unwrap();
        assert!(max * 2 < sum, "{source:?} max {max} sum {sum}");
        let mut week_sum = 0u64;
        let mut week_n = 0u64;
        let mut end_sum = 0u64;
        let mut end_n = 0u64;
        for row in &points {
            if (row.day + 3).rem_euclid(7) >= 5 {
                end_sum += row.messages;
                end_n += 1;
            } else {
                week_sum += row.messages;
                week_n += 1;
            }
        }
        assert!(end_n > 0 && week_n > 0);
        assert!(
            end_sum * week_n < week_sum * end_n,
            "{source:?} weekend {end_sum}/{end_n} weekday {week_sum}/{week_n}"
        );
        sum
    }

    fn assert_heatmap_shape(cells: &[hub_web::HeatCell]) {
        let hour = |want: u8| -> u64 {
            cells
                .iter()
                .filter(|cell| cell.hour == want)
                .map(|cell| cell.messages)
                .sum()
        };
        let office: u64 = (8..18).map(hour).sum();
        let other: u64 = (0..8).chain(18..24).map(hour).sum();
        assert!(office > other * 2, "office {office} other {other}");
        assert!(
            hour(12) < hour(11),
            "lunch {} late morning {}",
            hour(12),
            hour(11)
        );
    }

    fn assert_response_shape(rows: &[hub_web::DailyResponse]) {
        assert!(rows.iter().any(|row| row.sample == 0));
        assert!(
            rows.iter()
                .any(|row| row.low_sample && (1..5).contains(&row.sample))
        );
        let medians: Vec<f64> = rows.iter().filter_map(|row| row.p50_minutes).collect();
        assert!(medians.iter().any(|value| *value < 18.0));
        assert!(medians.iter().any(|value| *value > 18.0));
        assert!(medians.windows(2).any(|pair| pair[0] != pair[1]));
    }

    fn spark_sum(rows: &[hub_web::ConversationRow], title: &str) -> u64 {
        rows.iter()
            .find(|row| row.title == title)
            .unwrap()
            .messages_7d
            .iter()
            .sum()
    }

    fn assert_sparklines(telegram: &[hub_web::ConversationRow], zalo: &[hub_web::ConversationRow]) {
        let lit = telegram
            .iter()
            .chain(zalo.iter())
            .filter(|row| row.messages_7d.iter().any(|count| *count > 0))
            .count();
        assert!(lit >= 10, "lit {lit}");
        for title in [
            "Nhóm trực ca đêm",
            "Nhóm thông báo kho",
            "Tổ Kho Tân Bình",
            "Tổ Kho Thủ Đức",
        ] {
            assert_eq!(spark_sum(zalo, title), 0, "{title}");
        }
        for (rows, title) in [
            (zalo, "Nhóm Kho Quận 7"),
            (zalo, "Nhóm CSKH đối tác"),
            (zalo, "Anh Hùng (DM)"),
            (telegram, "Backend team"),
            (telegram, "Release room"),
            (telegram, "Minh (DM)"),
        ] {
            let sum = spark_sum(rows, title);
            assert!((20..=1_200).contains(&sum), "{title} {sum}");
        }
    }

    fn count_of(rows: &[hub_web::SourceCount], source: Source) -> u64 {
        rows.iter()
            .find(|row| row.source == source)
            .map(|row| row.messages)
            .unwrap_or(0)
    }

    fn label_of(rows: &[hub_web::LabelCount], label: &str) -> u64 {
        rows.iter()
            .find(|row| row.label == label)
            .map(|row| row.count)
            .unwrap_or(0)
    }

    fn seed_demo(app: &App, now: i64) {
        let store = Store::open(&app.paths.store()).unwrap();
        let mut hub = Hub::open(&app.paths.store()).unwrap();
        demo::seed(&store, &mut hub, now).unwrap();
    }

    fn record(hub: &mut Hub, external_id: &str, at: i64) {
        let id = ConversationId {
            network: Network::Telegram,
            kind: ConversationKind::Group,
            external_id: external_id.to_owned(),
        };
        hub.upsert_conversation(&NewConversation {
            id: id.clone(),
            title: Some("Boundary".into()),
            topic_id: None,
            muted: false,
            tags: None,
            seen_at_unix_secs: at,
        })
        .unwrap();
        hub.record_message(&CountedMessage {
            conversation: id,
            at_unix_secs: at,
            direction: Direction::In,
            content_kind: ContentKind::Text,
        })
        .unwrap();
    }

    #[tokio::test]
    async fn seeded_demo_feeds_every_dashboard_page() {
        let tmp = TempDir::new("demo-api");
        let scope = Scope::parse(&demo::scope_document(), &Policy::default()).unwrap();
        let app = open_app(&tmp.0, scope);
        let now = now_unix();
        seed_demo(&app, now);
        let api = super::Adapter::new(Arc::clone(&app));

        let overview = api
            .overview(OverviewQuery {
                days: 14,
                source: SourceFilter::All,
            })
            .await
            .unwrap();
        assert_eq!(
            count_of(&overview.kpi.messages_24h, Source::Telegram),
            1_012
        );
        assert_eq!(count_of(&overview.kpi.messages_24h, Source::Zalo), 272);
        assert_eq!(overview.kpi.needs_attention, 6);
        assert_eq!(overview.kpi.response_p50_minutes, Some(18.0));
        assert_eq!(overview.kpi.response_sample, 187);
        assert_eq!(overview.kpi.conversations[0].source, Source::Telegram);
        assert_eq!(overview.kpi.conversations[0].watched, 119);
        assert_eq!(overview.kpi.conversations[1].source, Source::Zalo);
        assert_eq!(overview.kpi.conversations[1].watched, 39);
        assert_eq!(
            overview
                .series
                .iter()
                .filter(|row| row.source == Source::Telegram)
                .count(),
            14
        );
        assert_eq!(
            overview
                .series
                .iter()
                .filter(|row| row.source == Source::Zalo)
                .count(),
            14
        );
        let telegram_series = assert_daily_shape(&overview.series, Source::Telegram);
        let zalo_series = assert_daily_shape(&overview.series, Source::Zalo);
        assert_eq!(overview.needs_attention.len(), 6);
        assert_eq!(overview.needs_attention[0].title, "Backend team");
        assert_eq!(overview.modules.len(), 3);
        assert!(
            overview
                .modules
                .iter()
                .all(|module| module.state == ModuleState::Ok)
        );

        let analytics = api
            .analytics(AnalyticsQuery {
                days: 14,
                source: SourceFilter::All,
            })
            .await
            .unwrap();
        assert_eq!(analytics.heatmap.len(), 168);
        let heat: u64 = analytics.heatmap.iter().map(|cell| cell.messages).sum();
        assert_eq!(heat, telegram_series + zalo_series);
        assert_heatmap_shape(&analytics.heatmap);
        assert_eq!(analytics.funnel.queued, 6);
        assert_eq!(analytics.funnel.draft, "not available in P1");
        assert_eq!(label_of(&analytics.funnel.resolved, "answered"), 187);
        assert_eq!(
            label_of(&analytics.funnel.resolved, "answered_elsewhere"),
            44
        );
        assert_eq!(label_of(&analytics.funnel.resolved, "read_only"), 96);
        assert_eq!(label_of(&analytics.funnel.resolved, "unseen"), 79);
        let resolved: u64 = analytics.funnel.resolved.iter().map(|row| row.count).sum();
        assert_eq!(resolved, 406);
        assert_eq!(analytics.funnel.queued + resolved, 412);
        let samples: u64 = analytics.response_p50.iter().map(|row| row.sample).sum();
        assert_eq!(samples, 187);
        assert_response_shape(&analytics.response_p50);
        assert_eq!(analytics.top_conversations.len(), 8);
        assert!(
            analytics
                .top_conversations
                .windows(2)
                .all(|pair| pair[0].messages >= pair[1].messages)
        );
        assert!(analytics.top_conversations[0].messages > 600);
        assert!(analytics.top_conversations[7].messages > 150);
        let top_sources: Vec<_> = analytics
            .top_conversations
            .iter()
            .map(|row| row.source)
            .collect();
        assert!(
            top_sources.contains(&Source::Telegram) && top_sources.contains(&Source::Zalo),
            "{:?}",
            analytics
                .top_conversations
                .iter()
                .map(|row| (row.title.as_str(), row.source, row.messages))
                .collect::<Vec<_>>()
        );

        let telegram_rows = api
            .conversations(conv(SourceFilter::Telegram, None))
            .await
            .unwrap();
        assert_eq!(telegram_rows.total, 119);
        let backend = telegram_rows
            .rows
            .iter()
            .find(|row| row.title == "Backend team")
            .unwrap();
        assert_eq!(backend.external_id, "-1005550003");
        assert_eq!(backend.open_tickets, 3);
        assert_eq!(backend.messages_7d.len(), 7);
        let zalo_rows = api
            .conversations(conv(SourceFilter::Zalo, None))
            .await
            .unwrap();
        assert_eq!(zalo_rows.total, 41);
        assert_eq!(
            api.conversations(conv(SourceFilter::Zalo, Some(ConversationKindFilter::Dm)))
                .await
                .unwrap()
                .total,
            16
        );
        assert_eq!(
            api.conversations(conv(
                SourceFilter::Zalo,
                Some(ConversationKindFilter::Group)
            ))
            .await
            .unwrap()
            .total,
            25
        );
        assert_eq!(
            api.conversations(conv(SourceFilter::All, Some(ConversationKindFilter::Forum)))
                .await
                .unwrap()
                .total,
            0
        );
        let kho = zalo_rows
            .rows
            .iter()
            .find(|row| row.title == "Nhóm Kho Quận 7")
            .unwrap();
        assert!(kho.external_id.contains('…'));
        assert_sparklines(&telegram_rows.rows, &zalo_rows.rows);

        let telegram = api.telegram(DaysQuery { days: 14 }).await.unwrap();
        assert_eq!(telegram.scope.watched, 119);
        assert_eq!(telegram.triggers.len(), 4);
        assert_eq!(telegram.top_senders[0].name, "Quang");
        assert_eq!(telegram.top_senders[0].count, 40);
        assert_eq!(telegram.top_senders[1].name, "Thảo");
        assert_eq!(telegram.top_senders[1].count, 28);

        let zalo = api.zalo(DaysQuery { days: 14 }).await.unwrap();
        assert_eq!(zalo.bridge.state, "ok");
        assert_eq!(zalo.session.account, None);
        assert_eq!(zalo.session.storage, "sidecar");
        assert_eq!(zalo.session.state, "valid");
        assert_eq!(zalo.send_queue.waiting, 0);
        assert_eq!(zalo.send_queue.rate_limits_24h, 1);
        assert_eq!(zalo.send_queue.retry_after_secs, Some(30));
        assert_eq!(zalo.pending_friend_requests, 3);
        assert_eq!(zalo.failed_events.len(), 4);
        assert_eq!(zalo.groups_without_topic.count, 7);
        assert!(
            zalo.groups_without_topic
                .names
                .iter()
                .any(|name| name == "Tổ Kho Tân Bình")
        );
        assert!(
            zalo.groups_without_topic
                .names
                .iter()
                .any(|name| name == "Nhóm trực ca đêm")
        );
        assert!(
            zalo.topics.iter().any(|topic| {
                topic.title == "Nhóm Kho Quận 7" && topic.topic_id == Some(4_100)
            })
        );

        let events = api.events(50).await.unwrap();
        let kinds: Vec<&str> = events
            .events
            .iter()
            .map(|event| event.kind.as_str())
            .collect();
        assert_eq!(kinds[0], "rate_limited");
        let newest = events
            .events
            .iter()
            .map(|event| event.at_unix_secs)
            .max()
            .unwrap();
        let oldest = events
            .events
            .iter()
            .map(|event| event.at_unix_secs)
            .min()
            .unwrap();
        assert!(newest - oldest > 2 * 86_400);
        for kind in [
            "stream_drop",
            "listener_drop",
            "qr_login",
            "reconnected",
            "friend_request",
        ] {
            assert!(kinds.contains(&kind), "{kind}");
        }

        let modules = api.modules().await.unwrap();
        assert_eq!(modules.modules[0].module, "captain-daemon");
        assert_eq!(modules.modules[1].module, "zalo-bridge");
        assert_eq!(modules.modules[2].module, "bridge");

        let telegram_only = api
            .overview(OverviewQuery {
                days: 14,
                source: SourceFilter::Telegram,
            })
            .await
            .unwrap();
        assert_eq!(telegram_only.kpi.messages_24h.len(), 1);
        assert_eq!(telegram_only.kpi.messages_24h[0].messages, 1_012);
        assert_eq!(telegram_only.kpi.needs_attention, 6);
        let zalo_only = api
            .analytics(AnalyticsQuery {
                days: 14,
                source: SourceFilter::Zalo,
            })
            .await
            .unwrap();
        assert_eq!(zalo_only.funnel.queued, 0);
        assert!(zalo_only.funnel.resolved.iter().all(|row| row.count == 0));
        let zalo_overview = api
            .overview(OverviewQuery {
                days: 14,
                source: SourceFilter::Zalo,
            })
            .await
            .unwrap();
        assert_eq!(zalo_overview.kpi.messages_24h[0].messages, 272);
        assert_eq!(zalo_overview.kpi.needs_attention, 0);
        assert_eq!(zalo_overview.kpi.conversations[0].watched, 39);

        let blob = serde_json::to_string(&overview).unwrap()
            + &serde_json::to_string(&telegram_rows).unwrap()
            + &serde_json::to_string(&telegram).unwrap()
            + &serde_json::to_string(&zalo).unwrap()
            + &serde_json::to_string(&events).unwrap();
        assert!(!blob.contains(demo::DEMO_MESSAGE_TEXT));

        let history = api
            .history(HistoryRequest {
                chat: -1_005_550_003,
                regex: "seed".into(),
                scan_limit: 10,
                text: true,
            })
            .await;
        match history {
            Ok(body) => assert!(
                !serde_json::to_string(&body)
                    .unwrap()
                    .contains(demo::DEMO_MESSAGE_TEXT)
            ),
            Err(error) => assert!(!error.to_string().contains(demo::DEMO_MESSAGE_TEXT)),
        }
        assert!(matches!(
            api.zalo_relogin().await,
            Err(hub_web::Error::NotImplemented(_))
        ));
        assert!(matches!(
            api.zalo_retry(1).await,
            Err(hub_web::Error::NotImplemented(_))
        ));
    }

    #[tokio::test]
    async fn empty_store_returns_zeros_without_failing() {
        let tmp = TempDir::new("empty-api");
        let app = open_app(&tmp.0, Scope::default());
        let api = super::Adapter::new(app);
        let overview = api
            .overview(OverviewQuery {
                days: 2,
                source: SourceFilter::All,
            })
            .await
            .unwrap();
        assert_eq!(overview.kpi.needs_attention, 0);
        assert_eq!(overview.kpi.response_p50_minutes, None);
        assert_eq!(overview.kpi.response_sample, 0);
        assert_eq!(overview.series.len(), 4);
        assert!(overview.series.iter().all(|row| row.messages == 0));
        assert_eq!(overview.modules[0].state, ModuleState::Down);
        assert_eq!(overview.modules[0].detail, "no heartbeat");
        assert_eq!(overview.modules[1].detail, "no heartbeat");
        assert_eq!(overview.modules[2].detail, "no heartbeat");

        let analytics = api
            .analytics(AnalyticsQuery {
                days: 2,
                source: SourceFilter::Telegram,
            })
            .await
            .unwrap();
        assert_eq!(analytics.heatmap.len(), 168);
        assert!(analytics.heatmap.iter().all(|cell| cell.messages == 0));
        assert!(
            analytics
                .response_p50
                .iter()
                .all(|row| row.p50_minutes.is_none() && row.low_sample)
        );
        assert_eq!(
            api.conversations(conv(SourceFilter::All, None))
                .await
                .unwrap()
                .total,
            0
        );
        let zalo = api.zalo(DaysQuery { days: 2 }).await.unwrap();
        assert_eq!(zalo.bridge.state, "down");
        assert_eq!(zalo.bridge.detail, "no heartbeat");
        assert_eq!(zalo.session.storage, "none");
        assert_eq!(zalo.session.account, None);
        assert_eq!(zalo.send_queue.rate_limits_24h, 0);
        assert_eq!(zalo.pending_friend_requests, 0);
        assert!(api.events(10).await.unwrap().events.is_empty());
    }

    #[test]
    fn rolling_24h_drops_the_hour_before_the_window() {
        let tmp = TempDir::new("rolling");
        let app = open_app(&tmp.0, Scope::default());
        let now: i64 = 1_700_000_000;
        let end_hour = now.div_euclid(3_600);
        let mut hub = Hub::open(&app.paths.store()).unwrap();
        record(&mut hub, "boundary", end_hour * 3_600);
        record(&mut hub, "boundary", (end_hour - 23) * 3_600);
        record(&mut hub, "boundary", (end_hour - 24) * 3_600);
        drop(hub);
        let overview = overview_at(
            &app,
            OverviewQuery {
                days: 14,
                source: SourceFilter::Telegram,
            },
            now,
            0,
        )
        .unwrap();
        assert_eq!(count_of(&overview.kpi.messages_24h, Source::Telegram), 2);
    }

    #[test]
    fn local_offset_moves_the_day_and_the_heatmap_hour() {
        let tmp = TempDir::new("offset");
        let app = open_app(&tmp.0, Scope::default());
        let at = 1_700_000_000_i64.div_euclid(86_400) * 86_400 + 20 * 3_600;
        let offset = 420;
        let mut hub = Hub::open(&app.paths.store()).unwrap();
        record(&mut hub, "local-hour", at);
        drop(hub);
        let local = at + i64::from(offset) * 60;
        let local_day = local.div_euclid(86_400);
        let utc_day = at.div_euclid(86_400);
        assert_ne!(local_day, utc_day);
        let hour = u8::try_from(local.div_euclid(3_600).rem_euclid(24)).unwrap();
        let weekday = u8::try_from((local_day + 3).rem_euclid(7)).unwrap();
        let overview = overview_at(
            &app,
            OverviewQuery {
                days: 1,
                source: SourceFilter::Telegram,
            },
            at,
            offset,
        )
        .unwrap();
        assert_eq!(overview.series.len(), 1);
        assert_eq!(overview.series[0].day, local_day);
        assert_eq!(overview.series[0].messages, 1);
        let analytics = analytics_at(
            &app,
            AnalyticsQuery {
                days: 1,
                source: SourceFilter::Telegram,
            },
            at,
            offset,
        )
        .unwrap();
        let cell = analytics
            .heatmap
            .iter()
            .find(|cell| cell.messages == 1)
            .unwrap();
        assert_eq!(cell.hour, hour);
        assert_eq!(cell.weekday, weekday);
    }
}
