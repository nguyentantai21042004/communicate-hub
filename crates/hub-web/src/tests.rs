//! Route tests against a fake [`HubApi`](crate::HubApi). No store and no network.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::limits::{DRAFT_UNAVAILABLE, HEATMAP_WEEKDAYS, HOURS_PER_DAY};
use crate::model::*;
use crate::{Error, HubApi, ModuleHealth, ModuleState, routes};

#[derive(Clone, Copy)]
enum Mode {
    Seeded,
    Empty,
    BridgeDown,
}

#[derive(Clone, Default)]
struct Seen {
    overview_days: Option<u32>,
    overview_source: Option<SourceFilter>,
    conversation_limit: Option<u32>,
    event_limit: Option<u32>,
    history_text: Option<bool>,
}

#[derive(Clone)]
struct Fake {
    mode: Mode,
    seen: Arc<Mutex<Seen>>,
}

impl Fake {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            seen: Arc::new(Mutex::new(Seen::default())),
        }
    }

    fn remember(&self, update: impl FnOnce(&mut Seen)) {
        let Ok(mut seen) = self.seen.lock() else {
            return;
        };
        update(&mut seen);
    }
}

impl HubApi for Fake {
    async fn overview(&self, query: OverviewQuery) -> crate::Result<Overview> {
        self.remember(|seen| {
            seen.overview_days = Some(query.days);
            seen.overview_source = Some(query.source);
        });
        Ok(match self.mode {
            Mode::Seeded => seeded_overview(),
            Mode::Empty | Mode::BridgeDown => empty_overview(query),
        })
    }

    async fn conversations(&self, query: ConversationQuery) -> crate::Result<ConversationPage> {
        self.remember(|seen| seen.conversation_limit = Some(query.limit));
        Ok(match self.mode {
            Mode::Seeded => seeded_conversations(),
            Mode::Empty | Mode::BridgeDown => ConversationPage {
                rows: Vec::new(),
                total: 0,
            },
        })
    }

    async fn analytics(&self, query: AnalyticsQuery) -> crate::Result<Analytics> {
        Ok(match self.mode {
            Mode::Seeded => seeded_analytics(),
            Mode::Empty | Mode::BridgeDown => empty_analytics(query),
        })
    }

    async fn telegram(&self, _query: DaysQuery) -> crate::Result<TelegramPage> {
        Ok(match self.mode {
            Mode::Seeded => seeded_telegram(),
            Mode::Empty | Mode::BridgeDown => empty_telegram(),
        })
    }

    async fn history(&self, body: HistoryRequest) -> crate::Result<HistoryResponse> {
        self.remember(|seen| seen.history_text = Some(body.text));
        Ok(HistoryResponse {
            chat: body.chat,
            scanned: 2,
            oldest_scanned: Some(10),
            more_before: false,
            matches: vec![
                HistoryMatch {
                    id: 10,
                    date_unix: 20,
                    sender: Some("Ada".into()),
                    text: Some("secret phrase".into()),
                    link: Some("https://t.me/c/1/10".into()),
                },
                HistoryMatch {
                    id: 11,
                    date_unix: 21,
                    sender: None,
                    text: Some("secret phrase".into()),
                    link: None,
                },
            ],
        })
    }

    async fn zalo(&self, _query: DaysQuery) -> crate::Result<ZaloPage> {
        Ok(match self.mode {
            Mode::Seeded => seeded_zalo(),
            Mode::BridgeDown => bridge_down_zalo(),
            Mode::Empty => empty_zalo(),
        })
    }

    async fn modules(&self) -> crate::Result<ModuleList> {
        Ok(ModuleList {
            modules: match self.mode {
                Mode::Seeded => vec![
                    health("captain-daemon", ModuleState::Ok, ""),
                    health("zalo-bridge", ModuleState::Degraded, "retrying"),
                    health("bridge", ModuleState::Unknown, ""),
                ],
                Mode::Empty | Mode::BridgeDown => vec![
                    health("captain-daemon", ModuleState::Down, "daemon is not running"),
                    health(
                        "zalo-bridge",
                        ModuleState::Down,
                        "zalo bridge is not running",
                    ),
                    health("bridge", ModuleState::Unknown, "bridge is not wired"),
                ],
            },
        })
    }

    async fn events(&self, limit: u32) -> crate::Result<EventPage> {
        self.remember(|seen| seen.event_limit = Some(limit));
        let events = match self.mode {
            Mode::Seeded => vec![
                ModuleEvent {
                    at_unix_secs: 30,
                    module: "bridge".into(),
                    level: "warn".into(),
                    kind: "send_failed".into(),
                    detail: "topic 4".into(),
                },
                ModuleEvent {
                    at_unix_secs: 10,
                    module: "zalo-bridge".into(),
                    level: "info".into(),
                    kind: "ready".into(),
                    detail: String::new(),
                },
            ],
            Mode::Empty | Mode::BridgeDown => Vec::new(),
        };
        let keep = usize::try_from(limit).unwrap_or(0);
        Ok(EventPage {
            events: events.into_iter().take(keep).collect(),
        })
    }

    async fn zalo_relogin(&self) -> crate::Result<ReloginAck> {
        match self.mode {
            Mode::Seeded => Ok(ReloginAck { started: true }),
            Mode::Empty | Mode::BridgeDown => {
                Err(Error::NotImplemented("zalo QR login is not wired".into()))
            }
        }
    }

    async fn zalo_retry(&self, id: i64) -> crate::Result<RetryAck> {
        match self.mode {
            Mode::Seeded => Ok(RetryAck { id, accepted: true }),
            Mode::Empty | Mode::BridgeDown => {
                Err(Error::NotImplemented("zalo retry is not wired".into()))
            }
        }
    }
}

fn health(module: &str, state: ModuleState, detail: &str) -> ModuleHealth {
    ModuleHealth {
        module: module.into(),
        state,
        detail: detail.into(),
        updated_at_unix_secs: 1_700_000_000,
    }
}

fn seeded_overview() -> Overview {
    Overview {
        kpi: OverviewKpi {
            messages_24h: vec![
                SourceCount {
                    source: Source::Telegram,
                    messages: 4,
                },
                SourceCount {
                    source: Source::Zalo,
                    messages: 1,
                },
            ],
            needs_attention: 2,
            response_p50_minutes: Some(12.5),
            response_sample: 6,
            conversations: vec![
                ActiveWatched {
                    source: Source::Telegram,
                    active: 3,
                    watched: 5,
                },
                ActiveWatched {
                    source: Source::Zalo,
                    active: 1,
                    watched: 2,
                },
            ],
            module_summary: ModuleSummary {
                ok: 1,
                degraded: 1,
                down: 1,
                unknown: 0,
            },
        },
        series: vec![
            DailyMessages {
                day: 20_000,
                source: Source::Telegram,
                messages: 4,
            },
            DailyMessages {
                day: 20_000,
                source: Source::Zalo,
                messages: 1,
            },
        ],
        needs_attention: vec![AttentionRow {
            source: Source::Telegram,
            title: "Ada".into(),
            trigger: "mention".into(),
            age_secs: 90,
        }],
        modules: vec![health("captain-daemon", ModuleState::Ok, "")],
    }
}

fn empty_overview(query: OverviewQuery) -> Overview {
    let sources = query.source.sources();
    Overview {
        kpi: OverviewKpi {
            messages_24h: sources
                .iter()
                .map(|source| SourceCount {
                    source: *source,
                    messages: 0,
                })
                .collect(),
            needs_attention: 0,
            response_p50_minutes: None,
            response_sample: 0,
            conversations: sources
                .iter()
                .map(|source| ActiveWatched {
                    source: *source,
                    active: 0,
                    watched: 0,
                })
                .collect(),
            module_summary: ModuleSummary {
                ok: 0,
                degraded: 0,
                down: 0,
                unknown: 0,
            },
        },
        series: (0..query.days)
            .flat_map(|offset| {
                sources.iter().map(move |source| DailyMessages {
                    day: i64::from(offset),
                    source: *source,
                    messages: 0,
                })
            })
            .collect(),
        needs_attention: Vec::new(),
        modules: Vec::new(),
    }
}

fn seeded_conversations() -> ConversationPage {
    ConversationPage {
        total: 1,
        rows: vec![ConversationRow {
            source: Source::Telegram,
            title: "Ada".into(),
            external_id: "1234".into(),
            kind: "dm".into(),
            messages_7d: vec![0, 1, 0, 2, 0, 0, 1],
            last_activity_unix_secs: Some(1_700_000_100),
            open_tickets: 1,
            tag: Some("dm".into()),
        }],
    }
}

fn seeded_analytics() -> Analytics {
    let mut heatmap = empty_heatmap();
    heatmap[1].messages = 3;
    Analytics {
        heatmap,
        funnel: TicketFunnel {
            queued: 2,
            draft: DRAFT_UNAVAILABLE.into(),
            resolved: vec![LabelCount {
                label: "answered".into(),
                count: 4,
            }],
        },
        top_conversations: vec![TopConversation {
            source: Source::Telegram,
            title: "Ada".into(),
            messages: 9,
        }],
        response_p50: vec![DailyResponse {
            day: 20_000,
            p50_minutes: Some(8.0),
            sample: 6,
            low_sample: false,
        }],
    }
}

fn empty_heatmap() -> Vec<HeatCell> {
    let mut cells = Vec::new();
    for weekday in 0..HEATMAP_WEEKDAYS {
        for hour in 0..HOURS_PER_DAY {
            cells.push(HeatCell {
                weekday,
                hour,
                messages: 0,
            });
        }
    }
    cells
}

fn empty_analytics(query: AnalyticsQuery) -> Analytics {
    Analytics {
        heatmap: empty_heatmap(),
        funnel: TicketFunnel {
            queued: 0,
            draft: DRAFT_UNAVAILABLE.into(),
            resolved: Vec::new(),
        },
        top_conversations: Vec::new(),
        response_p50: (0..query.days)
            .map(|offset| DailyResponse {
                day: i64::from(offset),
                p50_minutes: None,
                sample: 0,
                low_sample: true,
            })
            .collect(),
    }
}

fn seeded_telegram() -> TelegramPage {
    TelegramPage {
        connection: TelegramConnection {
            uptime: Some(0.99),
            gaps_unfilled: 1,
            lag_p50_secs: Some(2.0),
            lag_p99_secs: Some(11.0),
            last_catch_up_unix_secs: Some(1_700_000_050),
            reconnects_24h: 2,
        },
        session: TelegramSession {
            account: Some("Tai".into()),
            storage: "keychain".into(),
            connection: "connected".into(),
        },
        triggers: vec![TriggerStat {
            trigger: "mention".into(),
            count: 3,
            open: 1,
            resolved_ratio: Some(0.5),
            route: Some("draft".into()),
        }],
        scope: ScopeFunnel {
            watched: 4,
            with_messages: 3,
            with_tickets: 2,
            with_open_tickets: 1,
        },
        candidates: vec![ScopeCandidate {
            title: "Fleet".into(),
            would_trigger: 5,
            messages: 8,
            snippet: "[[chat]]\nid = 42\n".into(),
        }],
        top_senders: vec![SenderCount {
            name: "Ada".into(),
            count: 6,
        }],
    }
}

fn empty_telegram() -> TelegramPage {
    TelegramPage {
        connection: TelegramConnection {
            uptime: None,
            gaps_unfilled: 0,
            lag_p50_secs: None,
            lag_p99_secs: None,
            last_catch_up_unix_secs: None,
            reconnects_24h: 0,
        },
        session: TelegramSession {
            account: None,
            storage: "keychain".into(),
            connection: "offline".into(),
        },
        triggers: ["dm_all", "mention", "reply_to_me", "keyword"]
            .into_iter()
            .map(|trigger| TriggerStat {
                trigger: trigger.into(),
                count: 0,
                open: 0,
                resolved_ratio: None,
                route: None,
            })
            .collect(),
        scope: ScopeFunnel {
            watched: 0,
            with_messages: 0,
            with_tickets: 0,
            with_open_tickets: 0,
        },
        candidates: Vec::new(),
        top_senders: Vec::new(),
    }
}

fn seeded_zalo() -> ZaloPage {
    ZaloPage {
        bridge: BridgeCard {
            state: "ok".into(),
            detail: String::new(),
        },
        session: ZaloSession {
            account: Some("An".into()),
            storage: "file".into(),
            state: "valid".into(),
        },
        send_queue: SendQueue {
            waiting: 0,
            rate_limits_24h: 0,
            retry_after_secs: None,
        },
        topics: vec![TopicMapRow {
            title: "Nhóm".into(),
            external_id: "abcd…wxyz".into(),
            topic_id: Some(7),
            muted: false,
        }],
        message_kinds_14d: vec![KindCount {
            kind: "text".into(),
            count: 12,
        }],
        failed_events: vec![FailedEvent {
            id: 9,
            reason: "timeout".into(),
            conversation: "Nhóm".into(),
            at_unix_secs: 1_700_000_200,
            retryable: true,
        }],
        pending_friend_requests: 1,
        groups_without_topic: GroupGap {
            count: 1,
            names: vec!["Chưa gắn".into()],
        },
    }
}

fn bridge_down_zalo() -> ZaloPage {
    let mut page = seeded_zalo();
    page.bridge = BridgeCard {
        state: "down".into(),
        detail: "sidecar stopped".into(),
    };
    page.send_queue.waiting = 3;
    page.send_queue.rate_limits_24h = 2;
    page.send_queue.retry_after_secs = Some(15);
    page
}

fn empty_zalo() -> ZaloPage {
    ZaloPage {
        bridge: BridgeCard {
            state: "down".into(),
            detail: "zalo bridge is not running".into(),
        },
        session: ZaloSession {
            account: None,
            storage: "none".into(),
            state: "unknown".into(),
        },
        send_queue: SendQueue {
            waiting: 0,
            rate_limits_24h: 0,
            retry_after_secs: None,
        },
        topics: Vec::new(),
        message_kinds_14d: Vec::new(),
        failed_events: Vec::new(),
        pending_friend_requests: 0,
        groups_without_topic: GroupGap {
            count: 0,
            names: Vec::new(),
        },
    }
}

fn router(mode: Mode) -> (Router, Fake) {
    let fake = Fake::new(mode);
    (routes(fake.clone()), fake)
}

async fn call(mode: Mode, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Value) {
    let (app, _) = router(mode);
    send(app, method, uri, body).await
}

async fn send(app: Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let response = app
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or("").to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn paths(value: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(value, "", &mut out);
    out
}

fn walk(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.insert(path.clone());
                walk(child, &path, out);
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                walk(first, prefix, out);
            }
        }
        _ => {}
    }
}

fn assert_paths(value: &Value, expected: &[&str]) {
    let got = paths(value);
    let want: BTreeSet<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    assert_eq!(got, want);
}

const OVERVIEW_PATHS: &[&str] = &[
    "kpi",
    "kpi.messages_24h",
    "kpi.messages_24h.source",
    "kpi.messages_24h.messages",
    "kpi.needs_attention",
    "kpi.response_p50_minutes",
    "kpi.response_sample",
    "kpi.conversations",
    "kpi.conversations.source",
    "kpi.conversations.active",
    "kpi.conversations.watched",
    "kpi.module_summary",
    "kpi.module_summary.ok",
    "kpi.module_summary.degraded",
    "kpi.module_summary.down",
    "kpi.module_summary.unknown",
    "series",
    "series.day",
    "series.source",
    "series.messages",
    "needs_attention",
    "needs_attention.source",
    "needs_attention.title",
    "needs_attention.trigger",
    "needs_attention.age_secs",
    "modules",
    "modules.module",
    "modules.state",
    "modules.detail",
    "modules.updated_at_unix_secs",
];

const CONVERSATION_PATHS: &[&str] = &[
    "rows",
    "rows.source",
    "rows.title",
    "rows.external_id",
    "rows.kind",
    "rows.messages_7d",
    "rows.last_activity_unix_secs",
    "rows.open_tickets",
    "rows.tag",
    "total",
];

const ANALYTICS_PATHS: &[&str] = &[
    "heatmap",
    "heatmap.weekday",
    "heatmap.hour",
    "heatmap.messages",
    "funnel",
    "funnel.queued",
    "funnel.draft",
    "funnel.resolved",
    "funnel.resolved.label",
    "funnel.resolved.count",
    "top_conversations",
    "top_conversations.source",
    "top_conversations.title",
    "top_conversations.messages",
    "response_p50",
    "response_p50.day",
    "response_p50.p50_minutes",
    "response_p50.sample",
    "response_p50.low_sample",
];

const TELEGRAM_PATHS: &[&str] = &[
    "connection",
    "connection.uptime",
    "connection.gaps_unfilled",
    "connection.lag_p50_secs",
    "connection.lag_p99_secs",
    "connection.last_catch_up_unix_secs",
    "connection.reconnects_24h",
    "session",
    "session.account",
    "session.storage",
    "session.connection",
    "triggers",
    "triggers.trigger",
    "triggers.count",
    "triggers.open",
    "triggers.resolved_ratio",
    "triggers.route",
    "scope",
    "scope.watched",
    "scope.with_messages",
    "scope.with_tickets",
    "scope.with_open_tickets",
    "candidates",
    "candidates.title",
    "candidates.would_trigger",
    "candidates.messages",
    "candidates.snippet",
    "top_senders",
    "top_senders.name",
    "top_senders.count",
];

const ZALO_PATHS: &[&str] = &[
    "bridge",
    "bridge.state",
    "bridge.detail",
    "session",
    "session.account",
    "session.storage",
    "session.state",
    "send_queue",
    "send_queue.waiting",
    "send_queue.rate_limits_24h",
    "send_queue.retry_after_secs",
    "topics",
    "topics.title",
    "topics.external_id",
    "topics.topic_id",
    "topics.muted",
    "message_kinds_14d",
    "message_kinds_14d.kind",
    "message_kinds_14d.count",
    "failed_events",
    "failed_events.id",
    "failed_events.reason",
    "failed_events.conversation",
    "failed_events.at_unix_secs",
    "failed_events.retryable",
    "pending_friend_requests",
    "groups_without_topic",
    "groups_without_topic.count",
    "groups_without_topic.names",
];

const MODULE_PATHS: &[&str] = &[
    "modules",
    "modules.module",
    "modules.state",
    "modules.detail",
    "modules.updated_at_unix_secs",
];

const EVENT_PATHS: &[&str] = &[
    "events",
    "events.at_unix_secs",
    "events.module",
    "events.level",
    "events.kind",
    "events.detail",
];

#[tokio::test]
async fn seeded_responses_pin_field_names() {
    let (status, overview) = call(Mode::Seeded, "GET", "/overview?days=14&source=all", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&overview, OVERVIEW_PATHS);
    assert_eq!(overview["kpi"]["needs_attention"], 2);
    assert_eq!(overview["kpi"]["messages_24h"][0]["source"], "telegram");
    assert_eq!(overview["needs_attention"][0]["trigger"], "mention");
    assert!(!overview.to_string().contains("secret"));

    let (status, conversations) = call(Mode::Seeded, "GET", "/conversations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&conversations, CONVERSATION_PATHS);
    assert_eq!(
        conversations["rows"][0]["messages_7d"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert_eq!(conversations["total"], 1);

    let (status, analytics) = call(Mode::Seeded, "GET", "/analytics?days=7", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&analytics, ANALYTICS_PATHS);
    assert_eq!(analytics["heatmap"].as_array().unwrap().len(), 168);
    assert_eq!(analytics["funnel"]["draft"], DRAFT_UNAVAILABLE);
    assert_eq!(analytics["heatmap"][0]["weekday"], 0);
    assert_eq!(analytics["heatmap"][0]["hour"], 0);

    let (status, telegram) = call(Mode::Seeded, "GET", "/telegram", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&telegram, TELEGRAM_PATHS);
    assert_eq!(telegram["top_senders"][0].as_object().unwrap().len(), 2);
    assert!(
        telegram["candidates"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("id = 42")
    );

    let (status, zalo) = call(Mode::Seeded, "GET", "/zalo?days=14", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&zalo, ZALO_PATHS);
    assert_eq!(zalo["bridge"]["state"], "ok");

    let (status, modules) = call(Mode::Seeded, "GET", "/modules", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&modules, MODULE_PATHS);
    assert_eq!(
        modules,
        json!({
            "modules": [
                {"module": "captain-daemon", "state": "ok", "detail": "", "updated_at_unix_secs": 1700000000},
                {"module": "zalo-bridge", "state": "degraded", "detail": "retrying", "updated_at_unix_secs": 1700000000},
                {"module": "bridge", "state": "unknown", "detail": "", "updated_at_unix_secs": 1700000000}
            ]
        })
    );

    let (status, healthz) = call(Mode::Seeded, "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(healthz, modules);

    let (status, events) = call(Mode::Seeded, "GET", "/events?limit=1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_paths(&events, EVENT_PATHS);
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    assert_eq!(events["events"][0]["at_unix_secs"], 30);

    let (status, relogin) = call(Mode::Seeded, "POST", "/zalo/relogin", Some("{}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(relogin, json!({"started": true}));

    let (status, retry) = call(Mode::Seeded, "POST", "/zalo/retry/9", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retry, json!({"id": 9, "accepted": true}));
}

#[tokio::test]
async fn history_includes_text_only_when_asked_and_links_only_when_present() {
    let body = r#"{"chat":42,"regex":"fleet","scan_limit":10,"text":false}"#;
    let (status, page) = call(Mode::Seeded, "POST", "/history", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["chat"], 42);
    assert!(page["matches"][0].get("text").is_none());
    assert_eq!(page["matches"][0]["link"], "https://t.me/c/1/10");
    assert!(page["matches"][1].get("link").is_none());
    assert!(!page.to_string().contains("secret phrase"));

    let body = r#"{"chat":42,"regex":"fleet","scan_limit":10,"text":true}"#;
    let (status, page) = call(Mode::Seeded, "POST", "/history", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["matches"][0]["text"], "secret phrase");
}

#[tokio::test]
async fn empty_store_returns_zeros_and_bridge_down_keeps_numbers() {
    let (status, overview) =
        call(Mode::Empty, "GET", "/overview?days=2&source=telegram", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(overview["kpi"]["needs_attention"], 0);
    assert_eq!(overview["kpi"]["response_p50_minutes"], Value::Null);
    assert_eq!(overview["series"].as_array().unwrap().len(), 2);
    assert_eq!(overview["series"][0]["messages"], 0);
    assert_eq!(overview["needs_attention"].as_array().unwrap().len(), 0);

    let (status, conversations) = call(Mode::Empty, "GET", "/conversations", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(conversations["rows"].as_array().unwrap().len(), 0);
    assert_eq!(conversations["total"], 0);

    let (status, analytics) = call(Mode::Empty, "GET", "/analytics?days=3", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(analytics["heatmap"].as_array().unwrap().len(), 168);
    assert_eq!(analytics["funnel"]["queued"], 0);
    assert_eq!(analytics["funnel"]["draft"], DRAFT_UNAVAILABLE);
    assert_eq!(analytics["response_p50"].as_array().unwrap().len(), 3);
    assert_eq!(analytics["response_p50"][0]["low_sample"], true);

    let (status, telegram) = call(Mode::Empty, "GET", "/telegram", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(telegram["triggers"].as_array().unwrap().len(), 4);
    assert_eq!(telegram["triggers"][0]["trigger"], "dm_all");
    assert_eq!(telegram["scope"]["watched"], 0);
    assert_eq!(telegram["top_senders"].as_array().unwrap().len(), 0);

    let (status, zalo) = call(Mode::Empty, "GET", "/zalo", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(zalo["bridge"]["state"], "down");
    assert_eq!(zalo["send_queue"]["waiting"], 0);
    assert_eq!(zalo["pending_friend_requests"], 0);

    let (status, events) = call(Mode::Empty, "GET", "/events", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(events["events"].as_array().unwrap().len(), 0);

    let (status, down) = call(Mode::BridgeDown, "GET", "/zalo?days=14", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        down["bridge"],
        json!({"state": "down", "detail": "sidecar stopped"})
    );
    assert_eq!(down["send_queue"]["waiting"], 3);
    assert_eq!(down["send_queue"]["rate_limits_24h"], 2);
    assert_eq!(down["send_queue"]["retry_after_secs"], 15);
    assert_eq!(down["session"]["account"], "An");

    let (status, relogin) = call(Mode::Empty, "POST", "/zalo/relogin", Some("{}")).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(relogin["error"], "zalo QR login is not wired");

    let (status, retry) = call(Mode::Empty, "POST", "/zalo/retry/4", None).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(retry["error"], "zalo retry is not wired");
}

#[tokio::test]
async fn bad_queries_are_400_json_and_limits_clamp() {
    let cases = [
        ("/overview?source=sms", "source"),
        ("/overview?days=0", "days"),
        ("/overview?days=nope", "days"),
        ("/conversations?kind=room", "kind"),
        ("/conversations?limit=0", "limit"),
        ("/conversations?offset=-3", "offset"),
        ("/analytics?source=messenger", "source"),
        ("/telegram?days=-1", "days"),
        ("/events?limit=0", "limit"),
        ("/zalo?days=abc", "days"),
    ];
    for (uri, needle) in cases {
        let (status, body) = call(Mode::Empty, "GET", uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        let error = body["error"].as_str().unwrap_or("");
        assert!(error.contains(needle), "{uri}: {error}");
    }

    let (status, _) = call(Mode::Empty, "POST", "/history", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = call(
        Mode::Empty,
        "POST",
        "/history",
        Some(r#"{"chat":1,"regex":"","scan_limit":10}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "regex is empty");
    let (status, _) = call(
        Mode::Empty,
        "POST",
        "/history",
        Some(r#"{"chat":1,"regex":"a","extra":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = call(Mode::Empty, "POST", "/zalo/retry/nope", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "retry id must be an integer");

    let (app, fake) = router(Mode::Empty);
    let (status, _) = send(app, "GET", "/overview?days=9999&source=telegram", None).await;
    assert_eq!(status, StatusCode::OK);
    {
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.overview_days, Some(180));
        assert_eq!(seen.overview_source, Some(SourceFilter::Telegram));
    }

    let (app, fake) = router(Mode::Empty);
    let (status, _) = send(app, "GET", "/overview", None).await;
    assert_eq!(status, StatusCode::OK);
    {
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.overview_days, Some(14));
        assert_eq!(seen.overview_source, Some(SourceFilter::All));
    }

    let (app, fake) = router(Mode::Empty);
    let (status, _) = send(app, "GET", "/conversations?limit=99999", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fake.seen.lock().unwrap().conversation_limit, Some(200));

    let (app, fake) = router(Mode::Seeded);
    let (status, page) = send(app, "GET", "/events", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fake.seen.lock().unwrap().event_limit, Some(50));
    assert_eq!(page["events"].as_array().unwrap().len(), 2);

    let (app, fake) = router(Mode::Seeded);
    let body = r#"{"chat":1,"regex":"a","scan_limit":90000,"text":false}"#;
    let (status, _) = send(app, "POST", "/history", Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fake.seen.lock().unwrap().history_text, Some(false));
}
