//! JSON bodies the dashboard pages read.
//!
//! Field names are the wire contract. New fields may be added; existing names stay.
//! List bodies have no message text. Zalo ids arrive already masked.

use serde::Serialize;

use crate::ModuleHealth;

/// Network a row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Telegram, including Captain's own chats.
    Telegram,
    /// Zalo, reached through the bridge.
    Zalo,
}

impl Source {
    /// Wire name: `telegram` or `zalo`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Zalo => "zalo",
        }
    }
}

/// `source` query: one network, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFilter {
    /// Telegram and Zalo.
    All,
    /// Telegram only.
    Telegram,
    /// Zalo only.
    Zalo,
}

impl SourceFilter {
    /// Wire name: `all`, `telegram`, or `zalo`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Telegram => "telegram",
            Self::Zalo => "zalo",
        }
    }

    /// Networks this filter keeps, in display order.
    pub const fn sources(self) -> &'static [Source] {
        match self {
            Self::All => &[Source::Telegram, Source::Zalo],
            Self::Telegram => &[Source::Telegram],
            Self::Zalo => &[Source::Zalo],
        }
    }

    /// Whether a row of `source` belongs in the answer.
    pub const fn includes(self, source: Source) -> bool {
        match (self, source) {
            (Self::All, _) | (Self::Telegram, Source::Telegram) | (Self::Zalo, Source::Zalo) => {
                true
            }
            (Self::Telegram, Source::Zalo) | (Self::Zalo, Source::Telegram) => false,
        }
    }
}

/// `kind` query on the conversation list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKindFilter {
    /// A direct message.
    Dm,
    /// A group without topics.
    Group,
    /// A channel.
    Channel,
    /// A forum, or one topic.
    Forum,
}

impl ConversationKindFilter {
    /// Wire name: `dm`, `group`, `channel`, or `forum`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dm => "dm",
            Self::Group => "group",
            Self::Channel => "channel",
            Self::Forum => "forum",
        }
    }
}

/// Parsed `GET /api/overview` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewQuery {
    /// Window for the daily series, in days.
    pub days: u32,
    /// Which networks to include.
    pub source: SourceFilter,
}

/// Parsed `GET /api/conversations` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationQuery {
    /// Which networks to include.
    pub source: SourceFilter,
    /// Kind filter. `None` keeps every kind.
    pub kind: Option<ConversationKindFilter>,
    /// Keep rows active within this many days. `None` does not filter by time.
    pub days: Option<u32>,
    /// Case-insensitive title substring. `None` keeps every title.
    pub q: Option<String>,
    /// Page size, already clamped.
    pub limit: u32,
    /// Rows to skip, already clamped.
    pub offset: u32,
}

/// Parsed `GET /api/analytics` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnalyticsQuery {
    /// Window for the funnel, the ranking, and the daily response series.
    pub days: u32,
    /// Which networks to include.
    pub source: SourceFilter,
}

/// Parsed query that only carries `days`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaysQuery {
    /// Window length in days, already clamped.
    pub days: u32,
}

/// Body of `POST /api/history`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRequest {
    /// Telegram chat id to search.
    pub chat: i64,
    /// Case-insensitive regular expression. Empty is rejected.
    pub regex: String,
    /// Messages to scan, already clamped.
    pub scan_limit: usize,
    /// When false, matches must not carry message text.
    pub text: bool,
}

/// `GET /api/overview`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Overview {
    /// Headline numbers.
    pub kpi: OverviewKpi,
    /// One point per day and source, oldest day first.
    pub series: Vec<DailyMessages>,
    /// Oldest open tickets first, at most [`crate::limits::ATTENTION_LIMIT`].
    pub needs_attention: Vec<AttentionRow>,
    /// One entry per module, in strip order.
    pub modules: Vec<ModuleHealth>,
}

/// Headline numbers for the overview.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverviewKpi {
    /// Inbound messages on the current UTC day, one row per selected source.
    pub messages_24h: Vec<SourceCount>,
    /// Tickets still queued.
    pub needs_attention: u64,
    /// Median reply time in the window, in minutes. `None` when nobody replied.
    pub response_p50_minutes: Option<f64>,
    /// Replies behind `response_p50_minutes`.
    pub response_sample: u64,
    /// Active chats against watched chats, one row per selected source.
    pub conversations: Vec<ActiveWatched>,
    /// Counts of the module strip by state.
    pub module_summary: ModuleSummary,
}

/// A count tagged with its network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceCount {
    /// Network.
    pub source: Source,
    /// Messages.
    pub messages: u64,
}

/// Active chats and watched chats for one network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActiveWatched {
    /// Network.
    pub source: Source,
    /// Chats with an inbound message on the current UTC day.
    pub active: u64,
    /// Chats in scope.
    pub watched: u64,
}

/// How many modules are in each state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModuleSummary {
    /// Serving.
    pub ok: u64,
    /// Serving with a fault.
    pub degraded: u64,
    /// Not serving.
    pub down: u64,
    /// Not reported.
    pub unknown: u64,
}

/// Inbound messages of one source on one UTC day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyMessages {
    /// Days since the Unix epoch, UTC.
    pub day: i64,
    /// Network.
    pub source: Source,
    /// Inbound messages.
    pub messages: u64,
}

/// One ticket a person should look at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttentionRow {
    /// Network.
    pub source: Source,
    /// Conversation title. Never message text.
    pub title: String,
    /// Trigger id, for example `mention`.
    pub trigger: String,
    /// Seconds since the ticket opened.
    pub age_secs: i64,
}

/// `GET /api/conversations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationPage {
    /// The page, after `offset`.
    pub rows: Vec<ConversationRow>,
    /// Rows matching the filter, before the page.
    pub total: u64,
}

/// One conversation on the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationRow {
    /// Network.
    pub source: Source,
    /// Title. Never message text.
    pub title: String,
    /// Network id. Zalo ids are masked; Telegram ids are not.
    pub external_id: String,
    /// `dm`, `group`, `channel`, or `forum`.
    pub kind: String,
    /// Inbound messages for the last seven UTC days, oldest first.
    pub messages_7d: Vec<u64>,
    /// Latest known activity. `None` when the chat has never been seen.
    pub last_activity_unix_secs: Option<i64>,
    /// Tickets still queued or in the digest.
    pub open_tickets: u64,
    /// Owner tag, when the chat has one.
    pub tag: Option<String>,
}

/// `GET /api/analytics`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analytics {
    /// Weekday by UTC hour. Monday is 0. Always 7×24 cells.
    pub heatmap: Vec<HeatCell>,
    /// Where tickets in the window went.
    pub funnel: TicketFunnel,
    /// Busiest conversations in the window.
    pub top_conversations: Vec<TopConversation>,
    /// Median reply time per UTC day, oldest first.
    pub response_p50: Vec<DailyResponse>,
}

/// Messages in one weekday and hour, summed over the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HeatCell {
    /// 0 is Monday, 6 is Sunday.
    pub weekday: u8,
    /// 0 to 23, UTC.
    pub hour: u8,
    /// Inbound messages.
    pub messages: u64,
}

/// Ticket outcomes for the analytics window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketFunnel {
    /// Tickets still queued.
    pub queued: u64,
    /// Drafts are not produced in P1, so this is [`crate::limits::DRAFT_UNAVAILABLE`].
    pub draft: String,
    /// Resolved tickets by label.
    pub resolved: Vec<LabelCount>,
}

/// How many resolved tickets share one label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LabelCount {
    /// Implicit or explicit label id.
    pub label: String,
    /// Tickets.
    pub count: u64,
}

/// A conversation ranked by inbound messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopConversation {
    /// Network.
    pub source: Source,
    /// Title.
    pub title: String,
    /// Inbound messages in the window.
    pub messages: u64,
}

/// Median reply time on one UTC day.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DailyResponse {
    /// Days since the Unix epoch, UTC.
    pub day: i64,
    /// Median minutes. `None` when `sample` is zero.
    pub p50_minutes: Option<f64>,
    /// Replies that day.
    pub sample: u64,
    /// True when `sample` is below [`crate::limits::LOW_SAMPLE_MIN`].
    pub low_sample: bool,
}

/// `GET /api/telegram`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelegramPage {
    /// Link health for the window.
    pub connection: TelegramConnection,
    /// Signed-in account. Never a secret.
    pub session: TelegramSession,
    /// The four scope triggers, in a fixed order.
    pub triggers: Vec<TriggerStat>,
    /// How far watched chats got.
    pub scope: ScopeFunnel,
    /// Out-of-scope chats that would have triggered.
    pub candidates: Vec<ScopeCandidate>,
    /// Display name and count only.
    pub top_senders: Vec<SenderCount>,
}

/// Daemon link numbers for the Telegram page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelegramConnection {
    /// Connected time over the watched window. `None` when no run overlaps it.
    pub uptime: Option<f64>,
    /// Gaps between runs that catch-up did not fill.
    pub gaps_unfilled: u64,
    /// Median update lag, in seconds.
    pub lag_p50_secs: Option<f64>,
    /// 99th percentile update lag, in seconds.
    pub lag_p99_secs: Option<f64>,
    /// Start of the latest run that caught up.
    pub last_catch_up_unix_secs: Option<i64>,
    /// Successful reconnects in the last day.
    pub reconnects_24h: u64,
}

/// What the Telegram page may say about the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TelegramSession {
    /// Display name. `None` when the daemon is stopped.
    pub account: Option<String>,
    /// Where the session lives, for example `keychain`. Never the secret itself.
    pub storage: String,
    /// `connected`, `reconnecting`, `unauthorized`, or `offline`.
    pub connection: String,
}

/// One trigger row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TriggerStat {
    /// Trigger id: `dm_all`, `mention`, `reply_to_me`, or `keyword`.
    pub trigger: String,
    /// Tickets opened in the window.
    pub count: u64,
    /// Those still queued or in the digest.
    pub open: u64,
    /// Resolved tickets over `count`. `None` when `count` is zero.
    pub resolved_ratio: Option<f64>,
    /// Most common route, `draft` or `digest`.
    pub route: Option<String>,
}

/// Scope coverage for the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScopeFunnel {
    /// Chats in `scope.toml`.
    pub watched: u64,
    /// Watched chats with an inbound message in the window.
    pub with_messages: u64,
    /// Watched chats with a ticket in the window.
    pub with_tickets: u64,
    /// Watched chats with a queued or digest ticket.
    pub with_open_tickets: u64,
}

/// An out-of-scope chat, with a `scope.toml` snippet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScopeCandidate {
    /// Title, or `Untitled`.
    pub title: String,
    /// Messages that would have fired a trigger.
    pub would_trigger: u64,
    /// Messages counted.
    pub messages: u64,
    /// Exact Telegram id, inside a `[[chat]]` block the page can copy.
    pub snippet: String,
}

/// A sender on the Telegram page. No id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SenderCount {
    /// Display name.
    pub name: String,
    /// Messages.
    pub count: u64,
}

/// `GET /api/zalo`. A stopped bridge is this body with `bridge.state` of `down`, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZaloPage {
    /// Whether the bridge process is serving.
    pub bridge: BridgeCard,
    /// Signed-in account. Never a secret.
    pub session: ZaloSession,
    /// Outbound queue.
    pub send_queue: SendQueue,
    /// Conversation to forum topic.
    pub topics: Vec<TopicMapRow>,
    /// Payload kinds over the last 14 days.
    pub message_kinds_14d: Vec<KindCount>,
    /// Deliveries that failed.
    pub failed_events: Vec<FailedEvent>,
    /// Friend requests waiting.
    pub pending_friend_requests: u64,
    /// Groups that have no forum topic yet.
    pub groups_without_topic: GroupGap,
}

/// Bridge process, as the Zalo page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BridgeCard {
    /// `ok`, `degraded`, `down`, or `unknown`.
    pub state: String,
    /// Short reason. Empty when there is nothing to add.
    pub detail: String,
}

/// What the Zalo page may say about the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZaloSession {
    /// Display name.
    pub account: Option<String>,
    /// Storage kind. Never a cookie, token, or session string.
    pub storage: String,
    /// Login state: `valid`, `expired`, `logged_out`, `connecting`, or `unknown`.
    pub state: String,
}

/// Outbound Zalo queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendQueue {
    /// Messages waiting to send.
    pub waiting: u64,
    /// HTTP 429 responses in the last day.
    pub rate_limits_24h: u64,
    /// Seconds the latest 429 asked us to wait. `None` when there is no wait.
    pub retry_after_secs: Option<u64>,
}

/// One Zalo conversation and its Telegram topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopicMapRow {
    /// Title.
    pub title: String,
    /// Masked Zalo id.
    pub external_id: String,
    /// Forum topic id. `None` when the group has no topic.
    pub topic_id: Option<i64>,
    /// Whether the conversation is muted on Zalo, so the bridge may skip it.
    pub muted: bool,
}

/// How many messages had one payload kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KindCount {
    /// Kind id, for example `text` or `photo`.
    pub kind: String,
    /// Messages.
    pub count: u64,
}

/// One failed Zalo delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailedEvent {
    /// Row id, used by `POST /api/zalo/retry/{id}`.
    pub id: i64,
    /// Short reason. Not message text.
    pub reason: String,
    /// Conversation title, or a masked id when the title is missing.
    pub conversation: String,
    /// When it failed, Unix seconds.
    pub at_unix_secs: i64,
    /// Whether retry is meaningful.
    pub retryable: bool,
}

/// Groups that are not mapped to a topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupGap {
    /// How many.
    pub count: u64,
    /// Titles. Ids are not included.
    pub names: Vec<String>,
}

/// `GET /api/modules` and `GET /api/healthz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleList {
    /// `captain-daemon`, `zalo-bridge`, and `bridge`, in that order.
    pub modules: Vec<ModuleHealth>,
}

/// `GET /api/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventPage {
    /// Newest first.
    pub events: Vec<ModuleEvent>,
}

/// One module log line. `detail` is not message text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleEvent {
    /// When it happened, Unix seconds.
    pub at_unix_secs: i64,
    /// Module name.
    pub module: String,
    /// `debug`, `info`, `warn`, or `error`.
    pub level: String,
    /// Machine tag, for example `send_failed`.
    pub kind: String,
    /// Short note. Empty when there is none.
    pub detail: String,
}

/// `POST /api/history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryResponse {
    /// Chat that was searched.
    pub chat: i64,
    /// Messages read from Telegram.
    pub scanned: u64,
    /// Oldest message id that was read. `None` when nothing was read.
    pub oldest_scanned: Option<i64>,
    /// True when the scan stopped at `scan_limit` and older messages may match.
    pub more_before: bool,
    /// Matches, oldest first.
    pub matches: Vec<HistoryMatch>,
}

/// One history match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryMatch {
    /// Message id.
    pub id: i64,
    /// Send time, Unix seconds.
    pub date_unix: i64,
    /// Sender display name.
    pub sender: Option<String>,
    /// Message text. Omitted unless the request set `text` to true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `t.me` link. Omitted when Telegram has no public link for the chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// `POST /api/zalo/relogin` when the QR flow actually starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReloginAck {
    /// True when the QR flow was started.
    pub started: bool,
}

/// `POST /api/zalo/retry/{id}` when the bridge accepts the retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetryAck {
    /// Failed-event id.
    pub id: i64,
    /// True when the bridge queued another attempt.
    pub accepted: bool,
}
