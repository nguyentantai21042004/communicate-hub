//! `captain dev seed-demo`: two weeks of made-up activity, for working on the UI and the
//! report without a live account. Every name and message here is invented.
//!
//! The clock is the `now` argument. The generator does not read the wall clock. Message
//! hours use the machine's UTC offset so the daily bars and the heatmap match the pages.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use captain_core::{ChatId, ChatKind, ChatSummary, FormattedText, MessageId, MessageView};
use captain_inbox::{
    ChatEntry, EntryKind, ImplicitLabel, Policy, Route, Scope, ThreadKey, TicketState, TriggerKind,
};
use captain_store::{MetricEvent, MetricKind, NewTicket, Store, TicketFilter};
use chrono::TimeZone;
use hub_store::{
    ContentKind, ConversationId, ConversationKind, CountedMessage, Direction, Hub, LogLevel,
    ModuleHealth, ModuleState, Network, NewConversation, NewFailure, NewLogEvent,
};

/// Scope file written next to the demo store, matching the chats below.
pub const SCOPE_TOML: &str = r#"[[chat]]
id = 5550001
label = "Minh (DM)"
kind = "dm"
triggers = ["dm_all"]

[[chat]]
id = 5550002
label = "Lan (DM)"
kind = "dm"
triggers = ["dm_all"]

[[chat]]
id = -1005550003
label = "Backend team"
kind = "group"
triggers = ["mention", "reply_to_me", "keyword"]
keywords = ["deploy", "hotfix", "tài"]

[[chat]]
id = -1005550004
label = "Release room"
kind = "group"
triggers = ["mention", "reply_to_me"]

[[chat]]
id = -1005550005
label = "Platform forum"
kind = "forum"
triggers = ["mention", "reply_to_me", "keyword"]
keywords = ["incident"]
"#;

/// Message text stored for the sender sample. Dashboard responses must not echo it.
pub(crate) const DEMO_MESSAGE_TEXT: &str = "seed-body-should-not-leak";

const DAY_COUNT: usize = 14;
const DAYS: i64 = 14;
const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3_600;
const SECS_PER_MINUTE: i64 = 60;
const HOURS: usize = 24;
const ROLLING_HOURS: i64 = 24;
const RECENT_DAYS: i64 = 7; // sparkline length, oldest of the recent week included
const WEEKDAY_SHIFT: i64 = 3; // Unix day 0 is Thursday, Monday is 0
const TELEGRAM_WATCHED: usize = 119;
const NAMED_SCOPE_CHATS: usize = 5;
const EXTRA_SCOPE_FIRST_ID: i64 = -1_005_552_000;
const MINH_ID: i64 = 5_550_001;
const LAN_ID: i64 = 5_550_002;
const BACKEND_ID: i64 = -1_005_550_003;
const RELEASE_ID: i64 = -1_005_550_004;
const FORUM_ID: i64 = -1_005_550_005;
const NAMED_IDS: [i64; NAMED_SCOPE_CHATS] = [BACKEND_ID, RELEASE_ID, MINH_ID, LAN_ID, FORUM_ID];
const QUEUED_IDS: [i64; QUEUED] = [
    BACKEND_ID, RELEASE_ID, MINH_ID, LAN_ID, FORUM_ID, BACKEND_ID,
];
const DIGEST_IDS: [i64; DIGEST] = [BACKEND_ID, RELEASE_ID, FORUM_ID];
const QUEUED: usize = 6;
const DIGEST: usize = 3;
const ANSWERED: usize = 187;
const ANSWERED_ELSEWHERE: usize = 44;
const READ_ONLY: usize = 96;
const UNSEEN: usize = 79;
const RESOLVED: usize = ANSWERED + ANSWERED_ELSEWHERE + READ_ONLY + UNSEEN;
const READ_DELAY_SECS: i64 = 600;
const TELEGRAM_MESSAGES_24H: u64 = 1_012;
const ZALO_MESSAGES_24H: u64 = 272;
const ZALO_DMS: usize = 16;
const ZALO_GROUPS: usize = 25;
const ZALO_GAPS: usize = 7;
const ZALO_DM_ID_BASE: u64 = 848_000_000_000;
const ZALO_GROUP_ID_BASE: u64 = 849_000_000_000;
const TOPIC_BASE: i64 = 4_100;
const SENDER_MESSAGE_BASE: i32 = 100;
const TICKET_ANCHOR_BASE: i32 = 20_000;
const SENDER_SPAN_SECS: i64 = 3 * SECS_PER_DAY;
const SENDER_RECENT_SECS: i64 = 2 * SECS_PER_HOUR;
const TICKET_HOUR: i64 = 8;
const TICKET_GAP_SECS: i64 = 45;
const GAP_DAY: i64 = 9; // the daemon is down for part of this day
const GAP_HOURS: i64 = 5;
const SEEN_LAG_SECS: i64 = 2 * SECS_PER_DAY;
const MODULE_CAPTAIN: &str = "captain-daemon";
const MODULE_ZALO: &str = "zalo-bridge";
const MODULE_BRIDGE: &str = "bridge";
const EVENT_STREAM_DROP: &str = "stream_drop";
const EVENT_RECONNECTED: &str = "reconnected";
const EVENT_LISTENER_DROP: &str = "listener_drop";
const EVENT_QR_LOGIN: &str = "qr_login";
const EVENT_FRIEND_REQUEST: &str = "friend_request";
const EVENT_RATE_LIMITED: &str = "rate_limited";
const RATE_LIMIT_DETAIL: &str = "retry_after_secs=30";
const EVENT_RECONNECT_LAG: i64 = 180;
const ZALO_KIND_CYCLE: i64 = 5;
const ZALO_MUTED: usize = 2;
const STREAM_DROP_AGE: i64 = 2 * SECS_PER_DAY + 6 * SECS_PER_HOUR;
const LISTENER_DROP_AGE: i64 = SECS_PER_DAY + 12 * SECS_PER_HOUR;
const QR_LOGIN_AGE: i64 = 20 * SECS_PER_HOUR;
const RATE_LIMIT_AGE: i64 = 2 * SECS_PER_HOUR;
const FRIEND_REQUEST_AGES: [i64; 3] = [
    2 * SECS_PER_DAY,
    SECS_PER_DAY + 4 * SECS_PER_HOUR,
    8 * SECS_PER_HOUR,
];
const HOUR_SEARCH: i64 = 2; // how far to look for the UTC hour a local hour lands in
// Draft-route tickets per UTC day, oldest first. Scaled from the wireframe shape, one quiet day.
const DRAFT_BY_DAY: [usize; DAY_COUNT] = [10, 27, 25, 12, 6, 42, 37, 32, 80, 45, 0, 12, 57, 27];
const QUEUED_ON_DAY: [usize; DAY_COUNT] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 4];
// Answered replies per day. Days under 5 are low-sample. Day 10 is the empty gap.
// 93 replies are under 18 minutes and the next bucket is exactly 18, so the window p50 is 18.
const ANSWERED_BY_DAY: [usize; DAY_COUNT] = [8, 16, 14, 4, 4, 20, 27, 13, 30, 20, 0, 3, 16, 12];
const REPLY_MINUTES_BY_DAY: [i64; DAY_COUNT] =
    [12, 15, 11, 14, 16, 10, 17, 18, 28, 25, 0, 35, 22, 42];
const DIGEST_DAYS: [usize; DIGEST] = [8, 12, 13];
// Monday through Sunday. Weekends are the dip. The rolling 24h budget replaces part of the latest days.
const TG_BY_WEEKDAY: [u64; 7] = [860, 1_120, 740, 1_280, 980, 210, 140];
const ZL_BY_WEEKDAY: [u64; 7] = [280, 360, 240, 420, 300, 60, 40];
// Local hour weights. Office hours dominate, noon dips, nights stay lit.
const HOUR_WEIGHT: [u32; HOURS] = [
    1, 1, 1, 1, 1, 1, 1, 2, 6, 8, 10, 9, 3, 8, 10, 9, 8, 6, 3, 2, 1, 1, 1, 1,
];

const OUT_OF_SCOPE: [(i64, &str); 3] = [
    (-1_005_550_101, "Company announcements"),
    (-1_005_550_102, "Football Friday"),
    (5_550_103, "Huy"),
];
const SENDERS: [(&str, i64, u64); 4] = [
    ("Quang", 5_550_011, 40),
    ("Thảo", 5_550_012, 28),
    ("Dũng", 5_550_013, 15),
    ("Vy", 5_550_014, 9),
];
const FRIEND_NAMES: [&str; 3] = ["Anh Tuấn", "Chị Hạnh", "Anh Bình"];
const FILLER_STEMS: [&str; 8] = [
    "Nhóm kho",
    "Nhóm giao",
    "Tổ vận hành",
    "Nhóm đối tác",
    "Nhóm ca",
    "Phòng ban",
    "Nhóm khu vực",
    "Tổ điều phối",
];
// (label, older-week weight, recent-week weight). The older week is top-heavy. The recent week is flatter.
const TG_WEIGHT: &[(&str, u32, u32)] = &[
    ("Backend team", 34, 4),
    ("Release room", 20, 4),
    ("Platform forum", 14, 4),
    ("Minh (DM)", 10, 4),
    ("Lan (DM)", 8, 4),
    ("Quang (DM)", 6, 4),
    ("Hoa (DM)", 4, 3),
    ("Trang (DM)", 2, 3),
    ("Nhóm Incident", 2, 3),
    ("Ops hàng ngày", 0, 2),
    ("Nhóm QA", 0, 2),
    ("Nhóm Oncall", 0, 2),
    ("Anh Dũng (DM)", 0, 2),
    ("Chị Vy (DM)", 0, 2),
    ("Nhóm Triển khai", 0, 2),
    ("Nhóm Data", 0, 2),
    ("Phòng Kinh doanh", 0, 2),
    ("Nhóm Hỗ trợ ca 1", 0, 2),
    ("Nhóm Hỗ trợ ca 2", 0, 2),
    ("Nhóm Review", 0, 2),
];
const BUSY_EXTRA: &[ScopeChat] = &[
    ScopeChat {
        id: 5_550_006,
        label: "Quang (DM)",
        kind: "dm",
    },
    ScopeChat {
        id: 5_550_007,
        label: "Hoa (DM)",
        kind: "dm",
    },
    ScopeChat {
        id: 5_550_008,
        label: "Trang (DM)",
        kind: "dm",
    },
    ScopeChat {
        id: 5_550_009,
        label: "Anh Dũng (DM)",
        kind: "dm",
    },
    ScopeChat {
        id: 5_550_010,
        label: "Chị Vy (DM)",
        kind: "dm",
    },
    ScopeChat {
        id: -1_005_550_006,
        label: "Nhóm Incident",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_007,
        label: "Ops hàng ngày",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_008,
        label: "Nhóm QA",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_009,
        label: "Nhóm Oncall",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_010,
        label: "Nhóm Triển khai",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_011,
        label: "Nhóm Data",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_012,
        label: "Phòng Kinh doanh",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_013,
        label: "Nhóm Hỗ trợ ca 1",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_014,
        label: "Nhóm Hỗ trợ ca 2",
        kind: "group",
    },
    ScopeChat {
        id: -1_005_550_015,
        label: "Nhóm Review",
        kind: "group",
    },
];

struct ScopeChat {
    id: i64,
    label: &'static str,
    kind: &'static str,
}

struct ZaloSpec {
    title: &'static str,
    dm: bool,
    topic: bool,
    muted: bool,
    old_w: u32,
    recent_w: u32,
}

const fn zalo(
    title: &'static str,
    dm: bool,
    topic: bool,
    muted: bool,
    old_w: u32,
    recent_w: u32,
) -> ZaloSpec {
    ZaloSpec {
        title,
        dm,
        topic,
        muted,
        old_w,
        recent_w,
    }
}

const ZALO: &[ZaloSpec] = &[
    zalo("Anh Hùng (DM)", true, false, false, 16, 7),
    zalo("Chị Mai (DM)", true, false, false, 6, 5),
    zalo("Bác Tư (DM)", true, false, false, 4, 4),
    zalo("Anh Khoa (DM)", true, false, false, 1, 2),
    zalo("Cô Hạnh (DM)", true, false, false, 1, 2),
    zalo("Chị Trang (DM)", true, false, false, 1, 2),
    zalo("Anh Phong (DM)", true, false, false, 1, 2),
    zalo("Chị Vy (DM)", true, false, false, 1, 2),
    zalo("Anh Tú (DM)", true, false, false, 1, 2),
    zalo("Cô Nga (DM)", true, false, false, 1, 2),
    zalo("Anh Lộc (DM)", true, false, false, 1, 2),
    zalo("Chị Hà (DM)", true, false, false, 1, 2),
    zalo("Anh Nam (DM)", true, false, false, 1, 2),
    zalo("Chị Oanh (DM)", true, false, false, 1, 2),
    zalo("Anh Phát (DM)", true, false, false, 1, 2),
    zalo("Chị Yến (DM)", true, false, false, 1, 2),
    zalo("Nhóm Kho Quận 7", false, true, false, 48, 10),
    zalo("Nhóm CSKH đối tác", false, true, false, 26, 8),
    zalo("Nhóm Giao hàng Q2", false, true, false, 16, 6),
    zalo("Nhóm Khuyến mãi NCC", false, true, false, 6, 4),
    zalo("Nhóm Tin vặt khu phố", false, true, false, 4, 3),
    zalo("Nhóm Kho Quận 1", false, true, false, 2, 3),
    zalo("Nhóm Điều phối ca", false, true, false, 1, 2),
    zalo("Nhóm Kế toán nội bộ", false, true, false, 1, 2),
    zalo("Nhóm Đối tác Grab", false, true, false, 1, 2),
    zalo("Nhóm Kho Bình Tân", false, true, false, 1, 2),
    zalo("Nhóm CSKH Shopee", false, true, false, 1, 2),
    zalo("Nhóm Giao hàng Bình Thạnh", false, true, false, 1, 2),
    zalo("Nhóm Vận hành tối", false, true, false, 1, 2),
    zalo("Nhóm Tài xế nội thành", false, true, false, 1, 2),
    zalo("Nhóm Khiếu nại đơn", false, true, false, 1, 2),
    zalo("Nhóm Kho Quận 8", false, true, false, 1, 2),
    zalo("Nhóm CSKH nội bộ", false, true, false, 1, 2),
    zalo("Nhóm Giao gấp", false, true, false, 1, 2),
    zalo("Tổ Kho Tân Bình", false, false, false, 0, 0),
    zalo("Tổ Kho Thủ Đức", false, false, false, 0, 0),
    zalo("Nhóm trực ca đêm", false, false, true, 0, 0),
    zalo("Nhóm thông báo kho", false, false, true, 0, 0),
    zalo("Nhóm xe tải Q.8", false, false, false, 1, 2),
    zalo("Nhóm đối soát", false, false, false, 1, 2),
    zalo("Nhóm ca sáng", false, false, false, 1, 2),
];

/// The scope document `seed-demo` writes. Named chats, then enough groups to reach 119.
pub fn scope_document() -> String {
    let mut text = String::from(SCOPE_TOML);
    for chat in BUSY_EXTRA {
        push_chat(&mut text, chat.id, chat.label, chat.kind);
    }
    let named = NAMED_SCOPE_CHATS + BUSY_EXTRA.len();
    let fillers = TELEGRAM_WATCHED.saturating_sub(named);
    for index in 0..fillers {
        let id = EXTRA_SCOPE_FIRST_ID - i64::try_from(index).unwrap_or(0);
        let stem = FILLER_STEMS[index % FILLER_STEMS.len()];
        let label = format!("{stem} {}", index + 1);
        push_chat(&mut text, id, &label, "group");
    }
    text
}

fn push_chat(text: &mut String, id: i64, label: &str, kind: &str) {
    let triggers = if kind == "dm" {
        "\"dm_all\""
    } else {
        "\"mention\""
    };
    text.push_str(&format!(
        "\n[[chat]]\nid = {id}\nlabel = \"{label}\"\nkind = \"{kind}\"\ntriggers = [{triggers}]\n"
    ));
}

/// Seeds an empty store and the hub tables in the same file.
///
/// Returns how many tickets it made. A store that already has tickets is left unchanged.
///
/// # Errors
///
/// Returns an error when the store already has tickets, the scope text is invalid, or a write
/// fails.
pub fn seed(store: &Store, hub: &mut Hub, now: i64) -> Result<usize> {
    if !store.tickets(&TicketFilter::default())?.is_empty() {
        bail!("the store already has tickets; seed-demo only fills an empty one");
    }
    let scope = Scope::parse(&scope_document(), &Policy::default())?;
    if scope.entries().len() != TELEGRAM_WATCHED {
        bail!("scope has {} chats", scope.entries().len());
    }
    let today = now.div_euclid(SECS_PER_DAY);
    let start = (today - (DAYS - 1)) * SECS_PER_DAY;
    for day in 0..DAYS {
        let day_start = start + day * SECS_PER_DAY;
        runs(store, day, day_start, now)?;
        store.record(&MetricEvent {
            a: Some(250_000),
            b: Some(day),
            ..MetricEvent::new(MetricKind::StoreSize, day_start + SECS_PER_HOUR)
        })?;
        for (id, title) in OUT_OF_SCOPE {
            let chat = summary(id, title);
            for step in 0..8 {
                let would_trigger = id > 0 || step == 0;
                store.count_gate_drop(&chat, would_trigger, day_start + 10 * SECS_PER_HOUR)?;
            }
        }
    }
    store.record(&MetricEvent {
        a: Some(37),
        tag: Some("messages.getHistory".into()),
        ..MetricEvent::new(
            MetricKind::FloodWait,
            start + 3 * SECS_PER_DAY + 14 * SECS_PER_HOUR,
        )
    })?;
    let sender_ids = sender_messages(store, &scope, now)?;
    let tickets = write_tickets(store, &scope, now, &sender_ids)?;
    write_hub(hub, &scope, now)?;
    Ok(tickets)
}

/// One run per day, with a gap on [`GAP_DAY`] so uptime is not a flat 100%.
fn runs(store: &Store, day: i64, day_start: i64, now: i64) -> Result<()> {
    let end = (day_start + SECS_PER_DAY).min(now);
    if day == GAP_DAY {
        let first = store.start_run(day_start)?;
        store.stop_run(first, day_start + 6 * SECS_PER_HOUR, "signal")?;
        let second = store.start_run(day_start + (6 + GAP_HOURS) * SECS_PER_HOUR)?;
        store.stop_run(second, end, "signal")?;
        store.record(&MetricEvent {
            a: Some(37),
            b: Some(GAP_HOURS * SECS_PER_HOUR),
            tag: Some("ok".into()),
            ..MetricEvent::new(
                MetricKind::CatchUp,
                day_start + (6 + GAP_HOURS) * SECS_PER_HOUR,
            )
        })?;
        return Ok(());
    }
    let run = store.start_run(day_start)?;
    store.stop_run(run, end, "signal")?;
    Ok(())
}

fn sender_messages(store: &Store, scope: &Scope, now: i64) -> Result<Vec<MessageId>> {
    let entry = chat(scope, BACKEND_ID)?;
    let group = summary(BACKEND_ID, entry.label.trim_end_matches(" (DM)"));
    let end = now - SENDER_RECENT_SECS;
    store.upsert_chat(&group, Some(entry), end)?;
    let mut ids = Vec::new();
    let mut next = SENDER_MESSAGE_BASE;
    for (name, id, count) in SENDERS {
        let sender = summary(id, name);
        store.upsert_chat(&sender, None, end)?;
        let steps = i64::try_from(count).unwrap_or(1).max(1);
        for n in 0..count {
            let step = i64::try_from(n).unwrap_or(0);
            let at = if count <= 1 {
                end
            } else {
                end - SENDER_SPAN_SECS + SENDER_SPAN_SECS * step / (steps - 1)
            };
            let message_id = MessageId(next);
            next = next.checked_add(1).context("sender message id")?;
            store.put_message(
                &view(&group, message_id, &sender, at, DEMO_MESSAGE_TEXT, false),
                at,
                true,
            )?;
            ids.push(message_id);
        }
    }
    Ok(ids)
}

fn write_tickets(
    store: &Store,
    scope: &Scope,
    now: i64,
    sender_ids: &[MessageId],
) -> Result<usize> {
    check_ticket_plan()?;
    let oldest = now.div_euclid(SECS_PER_DAY) - (DAYS - 1);
    let mut made = 0usize;
    let mut index = 0usize;
    let mut resolved_n = 0usize;
    let mut queued_n = 0usize;
    let mut digest_n = 0usize;
    let mut elsewhere_left = ANSWERED_ELSEWHERE;
    let mut read_left = READ_ONLY;
    let mut unseen_left = UNSEEN;
    let mut attached = false;
    for day in 0..DAY_COUNT {
        let day_start = (oldest + i64::try_from(day).unwrap_or(0)) * SECS_PER_DAY;
        let draft = DRAFT_BY_DAY[day];
        let queued_here = QUEUED_ON_DAY[day];
        let answered_here = ANSWERED_BY_DAY[day];
        if queued_here + answered_here > draft {
            bail!("ticket day plan does not add up");
        }
        let resolved_here = draft - queued_here;
        let mut seq = 0usize;
        for n in 0..resolved_here {
            let label = if n < answered_here {
                ImplicitLabel::Answered
            } else {
                take_other(&mut elsewhere_left, &mut read_left, &mut unseen_left)?
            };
            let chat_id = NAMED_IDS[resolved_n % NAMED_IDS.len()];
            resolved_n += 1;
            let at = ticket_at(day_start, now, seq);
            seq += 1;
            let mut extra = Vec::new();
            // The newest Backend reply inside the last day carries the sender sample.
            if !attached && chat_id == BACKEND_ID && at >= now - SECS_PER_DAY {
                extra.extend_from_slice(sender_ids);
                attached = true;
            }
            let id = write_open(
                store,
                chat(scope, chat_id)?,
                &OpenPlan {
                    at,
                    state: TicketState::Queued,
                    route: Route::Draft,
                    state_reason: None,
                    index,
                    extra_messages: &extra,
                },
            )?;
            let reply_secs = REPLY_MINUTES_BY_DAY[day] * SECS_PER_MINUTE;
            let read_at = (label != ImplicitLabel::Unseen).then_some(at + READ_DELAY_SECS);
            let reply_at = (label == ImplicitLabel::Answered).then_some(at + reply_secs);
            store.resolve_ticket(
                id,
                label,
                read_at,
                reply_at,
                at + reply_secs + SECS_PER_MINUTE,
            )?;
            made += 1;
            index += 1;
        }
        for _ in 0..queued_here {
            let chat_id = QUEUED_IDS[queued_n];
            queued_n += 1;
            let at = ticket_at(day_start, now, seq);
            seq += 1;
            write_open(
                store,
                chat(scope, chat_id)?,
                &OpenPlan {
                    at,
                    state: TicketState::Queued,
                    route: Route::Draft,
                    state_reason: None,
                    index,
                    extra_messages: &[],
                },
            )?;
            made += 1;
            index += 1;
        }
        if DIGEST_DAYS.contains(&day) {
            let chat_id = DIGEST_IDS[digest_n];
            digest_n += 1;
            let at = ticket_at(day_start, now, seq);
            write_open(
                store,
                chat(scope, chat_id)?,
                &OpenPlan {
                    at,
                    state: TicketState::Digest,
                    route: Route::Digest,
                    state_reason: Some("keyword"),
                    index,
                    extra_messages: &[],
                },
            )?;
            made += 1;
            index += 1;
        }
    }
    if !attached || elsewhere_left != 0 || read_left != 0 || unseen_left != 0 {
        bail!("ticket day plan does not add up");
    }
    Ok(made)
}

fn check_ticket_plan() -> Result<()> {
    let draft: usize = DRAFT_BY_DAY.iter().copied().sum();
    let queued: usize = QUEUED_ON_DAY.iter().copied().sum();
    let answered: usize = ANSWERED_BY_DAY.iter().copied().sum();
    let days_match = usize::try_from(DAYS).ok() == Some(DAY_COUNT);
    if !days_match || draft != QUEUED + RESOLVED || queued != QUEUED || answered != ANSWERED {
        bail!("ticket day plan does not add up");
    }
    Ok(())
}

fn take_other(
    elsewhere: &mut usize,
    read_only: &mut usize,
    unseen: &mut usize,
) -> Result<ImplicitLabel> {
    if *elsewhere > 0 {
        *elsewhere -= 1;
        return Ok(ImplicitLabel::AnsweredElsewhere);
    }
    if *read_only > 0 {
        *read_only -= 1;
        return Ok(ImplicitLabel::ReadOnly);
    }
    if *unseen > 0 {
        *unseen -= 1;
        return Ok(ImplicitLabel::Unseen);
    }
    bail!("ticket day plan does not add up")
}

/// Past days open at 08:00. Today stays on today's UTC day and before `now`.
fn ticket_at(day_start: i64, now: i64, seq: usize) -> i64 {
    let limit = (day_start + SECS_PER_DAY).min(now);
    let seq_i = i64::try_from(seq).unwrap_or(0);
    let preferred = day_start + TICKET_HOUR * SECS_PER_HOUR + seq_i * TICKET_GAP_SECS;
    if day_start < limit && preferred < limit {
        return preferred;
    }
    let span = (limit - day_start).max(1);
    let at = day_start + seq_i.rem_euclid(span);
    if at < now { at } else { now.saturating_sub(1) }
}

struct OpenPlan<'a> {
    at: i64,
    state: TicketState,
    route: Route,
    state_reason: Option<&'a str>,
    index: usize,
    extra_messages: &'a [MessageId],
}

fn write_open(store: &Store, entry: &ChatEntry, plan: &OpenPlan<'_>) -> Result<i64> {
    let anchor = message_id(plan.index, TICKET_ANCHOR_BASE)?;
    let mut messages = vec![anchor];
    messages.extend_from_slice(plan.extra_messages);
    let (trigger, reason) = trigger_for(entry, plan.index, anchor.0);
    store
        .create_ticket(&NewTicket {
            key: ThreadKey {
                chat: entry.id,
                topic: None,
            },
            chat_label: entry.label.clone(),
            chat_kind: Some(entry.kind),
            profile: entry.profile.clone(),
            trigger,
            trigger_reason: reason,
            route: Some(plan.route),
            state: plan.state,
            state_reason: plan.state_reason.map(str::to_owned),
            stale: false,
            note: None,
            opened_at: plan.at,
            closed_at: plan.at + 30,
            anchor,
            anchor_date: plan.at,
            messages,
            senders: vec![ChatId(SENDERS[0].1)],
        })
        .map_err(Into::into)
}

fn write_hub(hub: &mut Hub, scope: &Scope, now: i64) -> Result<()> {
    check_zalo_plan()?;
    let chats = build_chats(hub, scope, now)?;
    place_messages(hub, &chats, now)?;
    bump_activity(hub, &chats, now)?;
    write_failures(hub, &chats, now)?;
    write_health(hub, now)?;
    write_events(hub, now)?;
    Ok(())
}

fn check_zalo_plan() -> Result<()> {
    let dms = ZALO.iter().filter(|spec| spec.dm).count();
    let groups = ZALO.iter().filter(|spec| !spec.dm).count();
    let muted = ZALO.iter().filter(|spec| spec.muted).count();
    let gaps = ZALO.iter().filter(|spec| !spec.dm && !spec.topic).count();
    if dms != ZALO_DMS || groups != ZALO_GROUPS || muted != ZALO_MUTED || gaps != ZALO_GAPS {
        bail!("zalo conversation plan does not match the wireframe counts");
    }
    Ok(())
}

struct SeedChat {
    row: i64,
    conv: ConversationId,
    title: String,
    topic_id: Option<i64>,
    muted: bool,
    tags: Option<String>,
    network: Network,
    old_w: u32,
    recent_w: u32,
}

fn build_chats(hub: &mut Hub, scope: &Scope, now: i64) -> Result<Vec<SeedChat>> {
    let mut weights = BTreeMap::new();
    for (label, old_w, recent_w) in TG_WEIGHT {
        weights.insert(*label, (*old_w, *recent_w));
    }
    let seen = now - SEEN_LAG_SECS;
    let mut chats = Vec::new();
    for entry in scope.entries() {
        let kind = if entry.kind == EntryKind::Dm {
            ConversationKind::Dm
        } else {
            ConversationKind::Group
        };
        let (old_w, recent_w) = weights.get(entry.label.as_str()).copied().unwrap_or((0, 0));
        let tags = (entry.id.0 == BACKEND_ID).then(|| "ops".to_owned());
        let conv = conversation(Network::Telegram, kind, &entry.id.0.to_string());
        let chat = SeedChat {
            row: 0,
            conv,
            title: entry.label.clone(),
            topic_id: None,
            muted: false,
            tags,
            network: Network::Telegram,
            old_w,
            recent_w,
        };
        let row = save_chat(hub, &chat, seen)?;
        chats.push(SeedChat { row, ..chat });
    }
    for (label, _, _) in TG_WEIGHT {
        if !chats.iter().any(|chat| chat.title == *label) {
            bail!("scope is missing weighted chat {label}");
        }
    }
    let mut dm_n = 0u64;
    let mut group_n = 0u64;
    let mut topic_n = 0i64;
    for spec in ZALO {
        let external = if spec.dm {
            dm_n += 1;
            ZALO_DM_ID_BASE + dm_n
        } else {
            group_n += 1;
            ZALO_GROUP_ID_BASE + group_n
        };
        let topic_id = if spec.topic {
            let id = TOPIC_BASE + topic_n;
            topic_n += 1;
            Some(id)
        } else {
            None
        };
        let kind = if spec.dm {
            ConversationKind::Dm
        } else {
            ConversationKind::Group
        };
        let chat = SeedChat {
            row: 0,
            conv: conversation(Network::Zalo, kind, &external.to_string()),
            title: spec.title.to_owned(),
            topic_id,
            muted: spec.muted,
            tags: None,
            network: Network::Zalo,
            old_w: spec.old_w,
            recent_w: spec.recent_w,
        };
        let row = save_chat(hub, &chat, seen)?;
        chats.push(SeedChat { row, ..chat });
    }
    Ok(chats)
}

fn save_chat(hub: &mut Hub, chat: &SeedChat, seen_at: i64) -> Result<i64> {
    hub.upsert_conversation(&NewConversation {
        id: chat.conv.clone(),
        title: Some(chat.title.clone()),
        topic_id: chat.topic_id,
        muted: chat.muted,
        tags: chat.tags.clone(),
        seen_at_unix_secs: seen_at,
    })
    .map_err(Into::into)
}

struct Clock {
    now: i64,
    offset: i32,
    utc_today: i64,
    local_today: i64,
    window_start: i64,
    window_end: i64,
}

struct HourSlot {
    at: i64,
    local_day: i64,
    hour: usize,
}

fn place_messages(hub: &mut Hub, chats: &[SeedChat], now: i64) -> Result<()> {
    let clock = clock_at(now);
    for network in [Network::Telegram, Network::Zalo] {
        // The oldest hour of the rolling window stays empty, so a seed that crosses an hour
        // boundary still reports the same 24h totals.
        let budget = budget_slots(&clock)?;
        let on_day = place_on(
            hub,
            chats,
            network,
            &budget,
            budget_total(network),
            clock.local_today,
        )?;
        let oldest = clock.local_today - (DAYS - 1);
        for back in 0..DAY_COUNT {
            let local_day = oldest + i64::try_from(back).unwrap_or(0);
            let already = on_day.get(&local_day).copied().unwrap_or(0);
            let gap = day_target(network, local_day).saturating_sub(already);
            if gap == 0 {
                continue;
            }
            let hours = history_slots(&clock, local_day);
            if hours.is_empty() {
                continue;
            }
            place_on(hub, chats, network, &hours, gap, clock.local_today)?;
        }
    }
    Ok(())
}

fn clock_at(now: i64) -> Clock {
    let offset = offset_minutes(now);
    let end_hour = now.div_euclid(SECS_PER_HOUR);
    Clock {
        now,
        offset,
        utc_today: now.div_euclid(SECS_PER_DAY),
        local_today: shifted_day(now, offset),
        window_start: end_hour - (ROLLING_HOURS - 1),
        window_end: end_hour,
    }
}

fn offset_minutes(now: i64) -> i32 {
    let minute = i32::try_from(SECS_PER_MINUTE).unwrap_or(1);
    match chrono::Local.timestamp_opt(now, 0) {
        chrono::LocalResult::Single(local) | chrono::LocalResult::Ambiguous(local, _) => {
            local.offset().local_minus_utc() / minute
        }
        chrono::LocalResult::None => 0,
    }
}

fn shift_secs(offset_minutes: i32) -> i64 {
    i64::from(offset_minutes).saturating_mul(SECS_PER_MINUTE)
}

fn shifted_day(unix: i64, offset_minutes: i32) -> i64 {
    unix.saturating_add(shift_secs(offset_minutes))
        .div_euclid(SECS_PER_DAY)
}

fn local_hour(unix: i64, offset_minutes: i32) -> usize {
    let local = unix.saturating_add(shift_secs(offset_minutes));
    let hour = local
        .div_euclid(SECS_PER_HOUR)
        .rem_euclid(i64::try_from(HOURS).unwrap_or(1));
    usize::try_from(hour).unwrap_or(0)
}

fn utc_visible(at: i64, utc_today: i64) -> bool {
    let day = at.div_euclid(SECS_PER_DAY);
    day >= utc_today - (DAYS - 1) && day <= utc_today
}

fn unix_for_local_hour(local_day: i64, hour: usize, offset_minutes: i32) -> Option<i64> {
    let want_hour = i64::try_from(hour).ok()?;
    let want = local_day
        .saturating_mul(SECS_PER_DAY)
        .saturating_add(want_hour.saturating_mul(SECS_PER_HOUR));
    let guess = want.saturating_sub(shift_secs(offset_minutes));
    let base = guess.div_euclid(SECS_PER_HOUR);
    for delta in -HOUR_SEARCH..=HOUR_SEARCH {
        let at = (base + delta) * SECS_PER_HOUR;
        if shifted_day(at, offset_minutes) == local_day && local_hour(at, offset_minutes) == hour {
            return Some(at);
        }
    }
    None
}

fn budget_slots(clock: &Clock) -> Result<Vec<HourSlot>> {
    let mut out = Vec::new();
    let mut index = clock.window_start + 1;
    while index <= clock.window_end {
        let at = index * SECS_PER_HOUR;
        if at < clock.now && utc_visible(at, clock.utc_today) {
            let local_day = shifted_day(at, clock.offset);
            if local_day >= clock.local_today - (DAYS - 1) && local_day <= clock.local_today {
                out.push(HourSlot {
                    at,
                    local_day,
                    hour: local_hour(at, clock.offset),
                });
            }
        }
        index += 1;
    }
    if out.is_empty() {
        bail!("no hour in the message window");
    }
    Ok(out)
}

fn history_slots(clock: &Clock, local_day: i64) -> Vec<HourSlot> {
    let mut out = Vec::new();
    for hour in 0..HOURS {
        let Some(at) = unix_for_local_hour(local_day, hour, clock.offset) else {
            continue;
        };
        if at >= clock.now {
            continue;
        }
        let index = at.div_euclid(SECS_PER_HOUR);
        if index >= clock.window_start && index <= clock.window_end {
            continue;
        }
        if !utc_visible(at, clock.utc_today) {
            continue;
        }
        out.push(HourSlot {
            at,
            local_day,
            hour,
        });
    }
    out
}

fn place_on(
    hub: &mut Hub,
    chats: &[SeedChat],
    network: Network,
    hours: &[HourSlot],
    total: u64,
    local_today: i64,
) -> Result<BTreeMap<i64, u64>> {
    let mut slots = Vec::new();
    for hour in hours {
        let Some(hour_w) = HOUR_WEIGHT.get(hour.hour).copied() else {
            continue;
        };
        if hour_w == 0 {
            continue;
        }
        for (index, chat) in chats.iter().enumerate() {
            if chat.network != network {
                continue;
            }
            let share = chat_share(chat, local_today, hour.local_day);
            let weight = share.saturating_mul(hour_w);
            if weight == 0 {
                continue;
            }
            slots.push((index, hour.at, hour.hour, hour.local_day, weight));
        }
    }
    let mut by_day = BTreeMap::new();
    if total == 0 {
        return Ok(by_day);
    }
    if slots.is_empty() {
        bail!("message counts were not placed");
    }
    let weights: Vec<u32> = slots.iter().map(|slot| slot.4).collect();
    let counts = spread(total, &weights);
    if counts.iter().sum::<u64>() != total {
        bail!("message counts were not placed");
    }
    for (slot, count) in slots.into_iter().zip(counts) {
        if count == 0 {
            continue;
        }
        record_n(hub, &chats[slot.0], slot.1, slot.2, count)?;
        *by_day.entry(slot.3).or_default() += count;
    }
    Ok(by_day)
}

fn spread(total: u64, weights: &[u32]) -> Vec<u64> {
    let sum: u64 = weights.iter().map(|weight| u64::from(*weight)).sum();
    if sum == 0 || total == 0 {
        return vec![0; weights.len()];
    }
    let mut counts = Vec::with_capacity(weights.len());
    let mut fractions = Vec::with_capacity(weights.len());
    let mut placed = 0u64;
    for (index, weight) in weights.iter().enumerate() {
        let weight = u64::from(*weight);
        let product = total.saturating_mul(weight);
        let share = product / sum;
        placed += share;
        counts.push(share);
        fractions.push((product % sum, index));
    }
    fractions.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut left = total.saturating_sub(placed);
    for (_, index) in fractions {
        if left == 0 {
            break;
        }
        if weights[index] == 0 {
            continue;
        }
        counts[index] += 1;
        left -= 1;
    }
    counts
}

fn record_n(hub: &mut Hub, chat: &SeedChat, at: i64, hour: usize, count: u64) -> Result<()> {
    let kind = content_kind(chat.network, hour);
    let mut left = count;
    while left > 0 {
        hub.record_message(&CountedMessage {
            conversation: chat.conv.clone(),
            at_unix_secs: at,
            direction: Direction::In,
            content_kind: kind,
        })?;
        left -= 1;
    }
    Ok(())
}

fn content_kind(network: Network, hour: usize) -> ContentKind {
    if network != Network::Zalo {
        return ContentKind::Text;
    }
    let cycle = usize::try_from(ZALO_KIND_CYCLE).unwrap_or(1).max(1);
    match hour % cycle {
        0 => ContentKind::Photo,
        1 => ContentKind::Voice,
        2 => ContentKind::File,
        3 => ContentKind::Sticker,
        _ => ContentKind::Text,
    }
}

fn budget_total(network: Network) -> u64 {
    match network {
        Network::Telegram => TELEGRAM_MESSAGES_24H,
        Network::Zalo => ZALO_MESSAGES_24H,
        Network::Messenger => 0,
    }
}

fn day_target(network: Network, local_day: i64) -> u64 {
    let index = weekday_index(local_day);
    match network {
        Network::Telegram => TG_BY_WEEKDAY[index],
        Network::Zalo => ZL_BY_WEEKDAY[index],
        Network::Messenger => 0,
    }
}

fn weekday_index(local_day: i64) -> usize {
    let weekday = (local_day + WEEKDAY_SHIFT).rem_euclid(7);
    usize::try_from(weekday).unwrap_or(0)
}

fn chat_share(chat: &SeedChat, local_today: i64, local_day: i64) -> u32 {
    if local_today - local_day < RECENT_DAYS {
        chat.recent_w
    } else {
        chat.old_w
    }
}

fn bump_activity(hub: &mut Hub, chats: &[SeedChat], now: i64) -> Result<()> {
    let mut ranked: Vec<&SeedChat> = chats
        .iter()
        .filter(|chat| chat.old_w > 0 || chat.recent_w > 0)
        .collect();
    ranked.sort_by(|left, right| {
        right
            .old_w
            .cmp(&left.old_w)
            .then(right.recent_w.cmp(&left.recent_w))
            .then(left.title.cmp(&right.title))
    });
    for (rank, chat) in ranked.iter().enumerate() {
        let lag = i64::try_from(rank).unwrap_or(0);
        save_chat(hub, chat, now - 1 - lag)?;
    }
    Ok(())
}

fn write_failures(hub: &mut Hub, chats: &[SeedChat], now: i64) -> Result<()> {
    let failures = [
        ("Nhóm Kho Quận 7", "send timed out", true),
        ("Anh Hùng (DM)", "session expired", false),
        ("Nhóm xe tải Q.8", "rate limited", true),
        ("Chị Mai (DM)", "user blocked", false),
    ];
    let recent = now - SECS_PER_HOUR;
    for (offset, (title, reason, retryable)) in failures.iter().enumerate() {
        let conversation_id = chats
            .iter()
            .find(|chat| chat.title == *title)
            .map(|chat| chat.row)
            .context("failure conversation")?;
        let lag = i64::try_from(offset).unwrap_or(0) * SECS_PER_MINUTE;
        hub.record_failure(&NewFailure {
            conversation_id: Some(conversation_id),
            reason: (*reason).to_owned(),
            retryable: *retryable,
            created_at_unix_secs: recent - lag,
        })?;
    }
    Ok(())
}

fn write_health(hub: &mut Hub, now: i64) -> Result<()> {
    let at = now - 30;
    for module in [MODULE_CAPTAIN, MODULE_ZALO, MODULE_BRIDGE] {
        hub.record_health(&ModuleHealth {
            module: module.to_owned(),
            state: ModuleState::Ok,
            detail: String::new(),
            updated_at_unix_secs: at,
        })?;
    }
    Ok(())
}

fn write_events(hub: &mut Hub, now: i64) -> Result<()> {
    let rows = [
        (
            MODULE_CAPTAIN,
            LogLevel::Warn,
            EVENT_STREAM_DROP,
            "update stream dropped",
            STREAM_DROP_AGE,
        ),
        (
            MODULE_CAPTAIN,
            LogLevel::Info,
            EVENT_RECONNECTED,
            "update stream restored",
            STREAM_DROP_AGE - EVENT_RECONNECT_LAG,
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_FRIEND_REQUEST,
            FRIEND_NAMES[0],
            FRIEND_REQUEST_AGES[0],
        ),
        (
            MODULE_ZALO,
            LogLevel::Warn,
            EVENT_LISTENER_DROP,
            "listener dropped",
            LISTENER_DROP_AGE,
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_RECONNECTED,
            "listener restored",
            LISTENER_DROP_AGE - EVENT_RECONNECT_LAG,
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_FRIEND_REQUEST,
            FRIEND_NAMES[1],
            FRIEND_REQUEST_AGES[1],
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_QR_LOGIN,
            "QR login completed",
            QR_LOGIN_AGE,
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_FRIEND_REQUEST,
            FRIEND_NAMES[2],
            FRIEND_REQUEST_AGES[2],
        ),
        (
            MODULE_ZALO,
            LogLevel::Info,
            EVENT_RATE_LIMITED,
            RATE_LIMIT_DETAIL,
            RATE_LIMIT_AGE,
        ),
    ];
    for (module, level, kind, detail, age) in rows {
        hub.record_module_event(&NewLogEvent {
            at_unix_secs: now - age,
            module: module.to_owned(),
            level,
            kind: kind.to_owned(),
            detail: Some(detail.to_owned()),
        })?;
    }
    Ok(())
}

fn conversation(network: Network, kind: ConversationKind, external_id: &str) -> ConversationId {
    ConversationId {
        network,
        kind,
        external_id: external_id.to_owned(),
    }
}

fn trigger_for(entry: &ChatEntry, index: usize, anchor: i32) -> (TriggerKind, String) {
    if entry.kind == EntryKind::Dm {
        return (TriggerKind::DmAll, "direct message".to_owned());
    }
    match index % 3 {
        0 => (
            TriggerKind::Mention,
            format!("mentioned in message {anchor}"),
        ),
        1 => (
            TriggerKind::ReplyToMe,
            format!("reply to your message in {anchor}"),
        ),
        _ => (TriggerKind::Keyword, "keyword deploy".to_owned()),
    }
}

fn chat(scope: &Scope, id: i64) -> Result<&ChatEntry> {
    scope
        .chat(ChatId(id))
        .context(format!("scope is missing chat {id}"))
}

fn message_id(index: usize, base: i32) -> Result<MessageId> {
    let offset = i32::try_from(index).context("message id")?;
    Ok(MessageId(base.checked_add(offset).context("message id")?))
}

fn summary(id: i64, name: &str) -> ChatSummary {
    ChatSummary {
        id: ChatId(id),
        kind: if id > 0 {
            ChatKind::User
        } else {
            ChatKind::Channel
        },
        name: Some(name.to_owned()),
        username: None,
        is_bot: false,
    }
}

fn view(
    chat: &ChatSummary,
    id: MessageId,
    sender: &ChatSummary,
    date: i64,
    text: &str,
    outgoing: bool,
) -> MessageView {
    MessageView {
        chat: chat.clone(),
        id,
        sender: Some(sender.clone()),
        outgoing,
        date_unix: date,
        text: FormattedText::plain(text),
        media: None,
        reply_to: None,
        album_id: None,
        mentioned: false,
        topic: None,
        quote_text: None,
        forward: None,
        edited_at_unix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_an_empty_store_once() {
        let dir = std::env::temp_dir().join(format!(
            "captain-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.sqlite");
        let store = Store::open(&path).unwrap();
        let mut hub = Hub::open(&path).unwrap();
        let now = 1_760_000_000;
        let n = seed(&store, &mut hub, now).unwrap();
        assert_eq!(n, QUEUED + DIGEST + RESOLVED);
        assert!(seed(&store, &mut hub, now).is_err());
        let input = store.report_input(now - DAYS * SECS_PER_DAY, now).unwrap();
        let report = captain_inbox::report::build(&input, &Policy::default());
        assert!(!report.sections.is_empty());

        let today = now.div_euclid(SECS_PER_DAY);
        let oldest = today - (DAYS - 1);
        let mut draft = [0usize; DAY_COUNT];
        let mut digest = 0usize;
        for ticket in store
            .tickets(&TicketFilter {
                limit: Some(1_000),
                ..TicketFilter::default()
            })
            .unwrap()
        {
            let day = ticket.opened_at.div_euclid(SECS_PER_DAY);
            let index = usize::try_from(day - oldest).unwrap();
            assert!(index < DAY_COUNT, "ticket day {day}");
            if ticket.route == Some(Route::Draft) {
                draft[index] += 1;
            } else if ticket.route == Some(Route::Digest) {
                digest += 1;
            }
        }
        assert_eq!(draft, DRAFT_BY_DAY);
        assert_eq!(digest, DIGEST);

        let rows = hub
            .reader()
            .counters_between_days(today - DAYS - 2, today + 1)
            .unwrap();
        let end_hour = now.div_euclid(SECS_PER_HOUR);
        let start_hour = end_hour - (ROLLING_HOURS - 1);
        let mut telegram = 0u64;
        let mut zalo = 0u64;
        let mut busy = std::collections::BTreeSet::new();
        let mut saw_telegram = false;
        let mut saw_zalo = false;
        for row in &rows {
            if row.count == 0 {
                continue;
            }
            busy.insert(row.conversation_id);
            let index = row.day * i64::try_from(HOURS).unwrap() + i64::from(row.hour);
            if (start_hour..=end_hour).contains(&index) {
                match row.network {
                    Network::Telegram => telegram += row.count,
                    Network::Zalo => zalo += row.count,
                    Network::Messenger => {}
                }
            }
            match row.network {
                Network::Telegram => saw_telegram = true,
                Network::Zalo => saw_zalo = true,
                Network::Messenger => {}
            }
        }
        assert_eq!(telegram, TELEGRAM_MESSAGES_24H);
        assert_eq!(zalo, ZALO_MESSAGES_24H);
        assert!(busy.len() >= 10, "busy {}", busy.len());
        assert!(saw_telegram);
        assert!(saw_zalo);

        let events = hub
            .reader()
            .recent_events(hub_store::EventStream::Module, 20)
            .unwrap();
        assert!(!events.is_empty());
        assert_eq!(events[0].kind, EVENT_RATE_LIMITED);
        let newest = events.iter().map(|event| event.at_unix_secs).max().unwrap();
        let oldest_event = events.iter().map(|event| event.at_unix_secs).min().unwrap();
        assert!(newest - oldest_event > 2 * SECS_PER_DAY);

        drop(store);
        drop(hub);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_document_lists_every_watched_chat() {
        let scope = Scope::parse(&scope_document(), &Policy::default()).unwrap();
        assert_eq!(scope.entries().len(), TELEGRAM_WATCHED);
    }
}
