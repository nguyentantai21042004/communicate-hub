//! Serde shapes the dashboard serializes as they stand. Field names are the JSON contract.

use serde::{Deserialize, Serialize};

use hub_store::{ContentKind, ConversationKind, LogLevel, ModuleHealth, Network};

/// Which networks a metric includes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Every network the store knows.
    All,
    /// Captain's own chats, plus bridge topics stored as Telegram.
    Telegram,
    /// Bridge topics stored as Zalo.
    Zalo,
}

impl Source {
    /// Whether `network` is part of this filter.
    pub fn includes(self, network: Network) -> bool {
        match self {
            Self::All => true,
            Self::Telegram => network == Network::Telegram,
            Self::Zalo => network == Network::Zalo,
        }
    }
}

/// Messages in one network over the overview's last 24 hours.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCount {
    /// Network the count belongs to.
    pub source: Network,
    /// Messages, both directions.
    pub count: u64,
}

/// Overview cards for one filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    /// Last 24 UTC hour buckets, including the hour of `now`. Zero rows are included.
    pub messages_24h: Vec<SourceCount>,
    /// Tickets currently `queued`, not limited to the range.
    pub queued_tickets: u64,
    /// Tickets currently `digest`, not limited to the range.
    pub digest_tickets: u64,
    /// Median minutes from anchor to the owner's reply, for resolved `answered` tickets in the range.
    pub response_p50_minutes: Option<f64>,
    /// Replies that went into `response_p50_minutes`.
    pub response_samples: u64,
    /// Unmuted conversations whose last activity falls in the range.
    pub active_conversations: u64,
    /// Unmuted conversations matching the source, ignoring the range.
    pub watched_conversations: u64,
    /// Latest snapshot of every module.
    pub modules: Vec<ModuleHealth>,
}

/// Messages on one UTC day for one network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailySourceCount {
    /// UTC day number since the Unix epoch.
    pub day: i64,
    /// Network.
    pub source: Network,
    /// Messages, both directions. Zero when the day had none.
    pub count: u64,
}

/// One cell of the weekday heatmap. Weekday 0 is Monday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatCell {
    /// 0 is Monday and 6 is Sunday, in the caller's timezone.
    pub weekday: u8,
    /// Hour 0 to 23 in the caller's timezone.
    pub hour: u8,
    /// Messages whose UTC hour bucket lands in this local hour.
    pub count: u64,
}

/// A 7 by 24 heatmap. `cells` is weekday-major, hour 0 to 23 inside each day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heatmap {
    /// Offset from UTC, in minutes, that was applied to each hour bucket.
    pub offset_minutes: i32,
    /// 168 cells. Index `weekday * 24 + hour`.
    pub cells: Vec<HeatCell>,
}

/// A conversation ranked by message count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedConversation {
    /// `conversations.id`.
    pub id: i64,
    /// Network.
    pub network: Network,
    /// Direct message or group.
    pub kind: ConversationKind,
    /// The network's own id.
    pub external_id: String,
    /// Display title.
    pub title: Option<String>,
    /// Messages in the window, both directions.
    pub messages: u64,
}

/// Drafts are not stored in phase 1, so the funnel says so instead of reporting zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStage {
    /// No draft table yet. Serializes as `not_available_in_p1`.
    NotAvailableInP1,
}

/// Resolved tickets split by Captain's implicit labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLabels {
    /// Replied in the same chat.
    pub answered: u64,
    /// Replied to the asker somewhere else.
    pub answered_elsewhere: u64,
    /// Read past the anchor without replying.
    pub read_only: u64,
    /// Neither read nor replied.
    pub unseen: u64,
    /// Resolved with no recognised label. The labelled counts plus this equal `resolved`.
    pub unlabelled: u64,
}

/// Queued, digest and resolved tickets opened in the window. Skipped tickets are omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketFunnel {
    /// Always [`DraftStage::NotAvailableInP1`] in this build.
    pub draft: DraftStage,
    /// Still `queued`.
    pub queued: u64,
    /// Still `digest`. Not included in `queued`.
    pub digest: u64,
    /// `resolved`.
    pub resolved: u64,
    /// How `resolved` splits.
    pub resolved_labels: ResolvedLabels,
}

/// Median reply time for one UTC day of anchor times.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DailyResponse {
    /// UTC day number of the anchor.
    pub day: i64,
    /// Median minutes. `None` when `samples` is 0.
    pub p50_minutes: Option<f64>,
    /// Resolved `answered` replies whose anchor falls on `day`.
    pub samples: u64,
    /// `true` when `samples` is below [`crate::MIN_DAILY_REPLIES`].
    pub low_sample: bool,
}

/// Filter for [`crate::conversations_page`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationFilter {
    /// Networks to include.
    pub source: Source,
    /// When set, only this kind.
    pub kind: Option<ConversationKind>,
    /// When set, last activity must fall in this many days before `now`.
    pub days: Option<i64>,
    /// Case-insensitive substring of the title. Empty and whitespace match everything.
    pub query: Option<String>,
}

/// One row of the conversations page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationItem {
    /// `conversations.id`.
    pub id: i64,
    /// Network.
    pub network: Network,
    /// Direct message or group.
    pub kind: ConversationKind,
    /// The network's own id.
    pub external_id: String,
    /// Display title.
    pub title: Option<String>,
    /// Telegram topic id, when this row is one topic.
    pub topic_id: Option<i64>,
    /// Whether triage ignores it.
    pub muted: bool,
    /// Earliest seen time, Unix seconds.
    pub first_seen_unix_secs: i64,
    /// Latest seen time, Unix seconds.
    pub last_activity_unix_secs: i64,
    /// Owner tags, as stored.
    pub tags: Option<String>,
    /// Message counts for the last 7 UTC days ending at `now`, oldest first.
    pub sparkline: Vec<u64>,
    /// Open `queued` and `digest` tickets attributed to this conversation.
    pub open_tickets: u64,
}

/// A page of conversations. `total` is the match count before `limit` and `offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationPage {
    /// Matches before paging.
    pub total: u64,
    /// Page size that was requested. Zero yields no items.
    pub limit: u64,
    /// Number of matches skipped.
    pub offset: u64,
    /// The page, most recently active first.
    pub items: Vec<ConversationItem>,
}

/// Messages of one content kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindCount {
    /// Payload kind.
    pub kind: ContentKind,
    /// Messages in the Zalo window.
    pub count: u64,
}

/// Zalo card: content mix, unresolved failures, and the topic-less group count the caller supplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZaloSummary {
    /// Calendar days covered by `kinds`. Always 14.
    pub window_days: i64,
    /// Every [`ContentKind`], including zeros.
    pub kinds: Vec<KindCount>,
    /// Unresolved failures on a Zalo conversation, plus failures with no conversation.
    pub failed_events: u64,
    /// Groups the caller says have no forum topic. This function does not compute it.
    pub topicless_groups: u64,
}

/// Tickets of one trigger, from Captain's M2 and M4 breakdowns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerBreakdown {
    /// Trigger name, as Captain stores it.
    pub trigger: String,
    /// Tickets of this trigger, excluding self-handled ones. This is M2's count.
    pub tickets: u64,
    /// Answered labels over labelled tickets. `None` when M4 has no row for this trigger.
    pub answered_ratio: Option<f64>,
}

/// Telegram trigger mix. `resolved_ratio` is M4's answered-over-labelled ratio, not resolved-over-all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramSummary {
    /// Tickets in the window after self-handled rows are dropped. This is M2's `n`.
    pub tickets: u64,
    /// Tickets that have an implicit label. This is M4's `n`.
    pub labelled: u64,
    /// M4 summary `all`. `None` when nothing was labelled.
    pub resolved_ratio: Option<f64>,
    /// One row per M2 trigger, in M2's order.
    pub triggers: Vec<TriggerBreakdown>,
}

/// How many log rows share one level. Missing levels are reported as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelCount {
    /// Severity.
    pub level: LogLevel,
    /// Rows in the last 24 hours.
    pub count: u64,
}

/// Health page: module snapshots, open failures, and log volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Latest snapshot of every module.
    pub modules: Vec<ModuleHealth>,
    /// Failures with no `resolved_at`.
    pub unresolved_failures: u64,
    /// Unresolved failures marked retryable.
    pub retryable_failures: u64,
    /// `bridge_events` in `[now - 86400, now)`, every level including zeros.
    pub bridge_events_24h: Vec<LevelCount>,
    /// `module_events` in the same window.
    pub module_events_24h: Vec<LevelCount>,
}

pub(crate) const NETWORKS: [Network; 3] = [Network::Telegram, Network::Zalo, Network::Messenger];
