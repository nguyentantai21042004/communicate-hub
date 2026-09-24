//! `captain dev seed-demo`: two weeks of made-up activity, for working on the UI and the
//! report without a live account. Every name and message here is invented.

use anyhow::{Result, bail};
use captain_core::{ChatId, ChatKind, ChatSummary, FormattedText, MessageId, MessageView};
use captain_inbox::report::SELF_HANDLED;
use captain_inbox::{
    ChatEntry, EntryKind, ExplicitLabel, ImplicitLabel, Policy, Route, Scope, ThreadKey,
    TicketState, TriggerKind,
};
use captain_store::{MetricEvent, MetricKind, NewTicket, Store, TicketFilter};

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

const DAYS: i64 = 14;
const SECS_PER_DAY: i64 = 86_400;
const SECS_PER_HOUR: i64 = 3600;
const SELF_ID: i64 = 5_550_000;
const FIRST_MESSAGE_ID: i32 = 100;
const GAP_DAY: i64 = 9; // the daemon is down for part of this day
const GAP_HOURS: i64 = 5;
const WORK_START_HOUR: i64 = 8;
const WORK_HOURS: u64 = 12;
const BYTES_PER_MESSAGE: i64 = 1_800;
const BASE_STORE_BYTES: i64 = 250_000;

const OUT_OF_SCOPE: [(i64, &str); 3] = [
    (-1_005_550_101, "Company announcements"),
    (-1_005_550_102, "Football Friday"),
    (5_550_103, "Huy"),
];
const SENDERS: [(i64, &str); 4] = [
    (5_550_011, "Quang"),
    (5_550_012, "Thảo"),
    (5_550_013, "Dũng"),
    (5_550_014, "Vy"),
];
const LINES: [&str; 8] = [
    "Có ai xem giúp PR này chưa?",
    "Build staging đang đỏ, ai đụng vào không?",
    "Chiều nay họp lúc 3h nhé",
    "Link dashboard mới đây",
    "Cái này có cần deploy lại không?",
    "Ok để mình check",
    "Hôm qua có incident ở payment",
    "Ai rảnh review giúp mình với",
];

/// Small deterministic generator, so every seed gives the same demo.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// Seeds an empty store and returns how many tickets it made.
pub fn seed(store: &Store, now: i64) -> Result<usize> {
    if !store.tickets(&TicketFilter::default())?.is_empty() {
        bail!("the store already has tickets; seed-demo only fills an empty one");
    }
    let scope = Scope::parse(SCOPE_TOML, &Policy::default())?;
    let mut rng = Rng(0x5eed_cafe);
    let mut next_id = FIRST_MESSAGE_ID;
    let mut tickets = 0;
    let start = (now - DAYS * SECS_PER_DAY).div_euclid(SECS_PER_DAY) * SECS_PER_DAY;

    for day in 0..DAYS {
        let day_start = start + day * SECS_PER_DAY;
        runs(store, day, day_start, now)?;
        for entry in scope.entries() {
            let bursts = match entry.kind {
                EntryKind::Dm => 1 + rng.below(3),
                EntryKind::Group | EntryKind::Forum => 2 + rng.below(4),
            };
            for _ in 0..bursts {
                let at = day_start
                    + (WORK_START_HOUR + i64::try_from(rng.below(WORK_HOURS)).unwrap_or(0))
                        * SECS_PER_HOUR
                    + i64::try_from(rng.below(3000)).unwrap_or(0);
                if at >= now {
                    continue;
                }
                if burst(store, entry, at, now, &mut rng, &mut next_id)? {
                    tickets += 1;
                }
            }
        }
        for (id, title) in OUT_OF_SCOPE {
            let chat = summary(id, title);
            for _ in 0..rng.below(20) {
                let would_trigger = rng.chance(if id > 0 { 90 } else { 5 });
                store.count_gate_drop(&chat, would_trigger, day_start + 10 * SECS_PER_HOUR)?;
            }
        }
        store.record(&MetricEvent {
            a: Some(BASE_STORE_BYTES + i64::from(next_id - FIRST_MESSAGE_ID) * BYTES_PER_MESSAGE),
            b: Some(i64::from(next_id - FIRST_MESSAGE_ID)),
            ..MetricEvent::new(MetricKind::StoreSize, day_start + SECS_PER_HOUR)
        })?;
        if rng.chance(20) {
            store.record(&MetricEvent {
                a: Some(i64::try_from(3 + rng.below(40)).unwrap_or(3)),
                tag: Some("messages.getHistory".into()),
                ..MetricEvent::new(MetricKind::FloodWait, day_start + 14 * SECS_PER_HOUR)
            })?;
        }
    }
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

/// Stores a burst of incoming messages and, usually, the ticket it made. Returns whether a
/// ticket was created.
fn burst(
    store: &Store,
    entry: &ChatEntry,
    at: i64,
    now: i64,
    rng: &mut Rng,
    next_id: &mut i32,
) -> Result<bool> {
    let chat = summary(entry.id.0, entry.label.trim_end_matches(" (DM)"));
    let chat = ChatSummary {
        kind: if entry.kind == EntryKind::Dm {
            ChatKind::User
        } else {
            ChatKind::Channel
        },
        ..chat
    };
    store.upsert_chat(&chat, Some(entry), at)?;
    let topic =
        (entry.kind == EntryKind::Forum).then(|| MessageId(1 + 10 * i32::from(rng.chance(50))));
    let key = ThreadKey {
        chat: entry.id,
        topic,
    };
    let count = 1 + rng.below(if entry.kind == EntryKind::Dm { 3 } else { 5 });
    let mut ids = Vec::new();
    let mut senders = Vec::new();
    let mut last = at;
    for i in 0..count {
        let sender = if entry.kind == EntryKind::Dm {
            chat.clone()
        } else {
            let (id, name) = SENDERS[usize::try_from(rng.below(4)).unwrap_or(0)];
            summary(id, name)
        };
        if sender.id != chat.id {
            store.upsert_chat(&sender, None, at)?;
        }
        let date = if i == 0 {
            at
        } else {
            last + i64::try_from(5 + rng.below(40)).unwrap_or(5)
        };
        last = date;
        let id = MessageId(*next_id);
        *next_id += 1;
        let line = LINES[usize::try_from(rng.below(8)).unwrap_or(0)];
        store.put_message(
            &view(&chat, id, &sender, date, line, topic, false),
            date,
            true,
        )?;
        ids.push(id);
        if !senders.contains(&sender.id) {
            senders.push(sender.id);
        }
    }
    let anchor = *ids.last().unwrap_or(&MessageId(0));

    let (kind, reason) = match entry.kind {
        EntryKind::Dm => (TriggerKind::DmAll, "direct message".to_owned()),
        _ => match rng.below(10) {
            0..=4 => (
                TriggerKind::Mention,
                format!("mentioned in message {}", anchor.0),
            ),
            5..=6 => (
                TriggerKind::ReplyToMe,
                format!("reply to your message in {}", anchor.0),
            ),
            _ if entry.enables(TriggerKind::Keyword) => {
                (TriggerKind::Keyword, "keyword deploy".to_owned())
            }
            // No trigger fired: the messages are stored, no ticket.
            _ => return Ok(false),
        },
    };

    let self_handled = rng.chance(8);
    let paused = !self_handled && rng.chance(4);
    let stale = !self_handled && !paused && rng.chance(3);
    let (state, route, state_reason) = if self_handled {
        (TicketState::Skipped, None, Some(SELF_HANDLED))
    } else if paused {
        (TicketState::Skipped, None, Some("paused"))
    } else if stale {
        (TicketState::Digest, Some(Route::Digest), Some("stale"))
    } else if kind == TriggerKind::Keyword {
        (TicketState::Digest, Some(Route::Digest), Some("keyword"))
    } else {
        (TicketState::Queued, Some(Route::Draft), None)
    };
    let closed_at = last + if entry.kind == EntryKind::Dm { 45 } else { 10 };
    let id = store.create_ticket(&NewTicket {
        key,
        chat_label: entry.label.clone(),
        chat_kind: Some(entry.kind),
        profile: entry.profile.clone(),
        trigger: kind,
        trigger_reason: reason,
        route,
        state,
        state_reason: state_reason.map(str::to_owned),
        stale,
        note: None,
        opened_at: at,
        closed_at,
        anchor,
        anchor_date: last,
        messages: ids,
        senders: senders.clone(),
    })?;
    let hops = i64::try_from(rng.below(6)).unwrap_or(0);
    store.record(&MetricEvent {
        chat: Some(entry.id),
        topic,
        ticket: Some(id),
        a: Some(hops),
        b: (hops > 0).then(|| hops * 600),
        ..MetricEvent::new(MetricKind::ContextShape, closed_at)
    })?;
    store.record(&MetricEvent {
        chat: Some(entry.id),
        topic,
        ticket: Some(id),
        a: Some(i64::try_from(rng.below(15)).unwrap_or(0)),
        b: Some(i64::try_from(10 + rng.below(80)).unwrap_or(0)),
        ..MetricEvent::new(MetricKind::ContextDensity, closed_at)
    })?;

    if state == TicketState::Skipped || now - last < SECS_PER_DAY {
        return Ok(true);
    }
    // Resolve older tickets the way the resolver would, leaving the owner's traces behind.
    let label = match rng.below(10) {
        0..=4 => ImplicitLabel::Answered,
        5 => ImplicitLabel::AnsweredElsewhere,
        6..=7 => ImplicitLabel::ReadOnly,
        _ => ImplicitLabel::Unseen,
    };
    let read_at = (label != ImplicitLabel::Unseen)
        .then(|| last + i64::try_from(60 + rng.below(3 * 3600)).unwrap_or(60));
    let reply_at = (label == ImplicitLabel::Answered)
        .then(|| read_at.unwrap_or(last) + i64::try_from(30 + rng.below(900)).unwrap_or(30));
    if let Some(read) = read_at {
        store.record(&MetricEvent {
            chat: Some(entry.id),
            topic,
            a: Some(i64::from(anchor.0)),
            b: Some(0),
            ..MetricEvent::new(MetricKind::ReadAdvanced, read)
        })?;
    }
    if let Some(reply) = reply_at {
        let me = summary(SELF_ID, "Tài");
        let reply_id = MessageId(*next_id);
        *next_id += 1;
        let mut m = view(&chat, reply_id, &me, reply, "Để mình xem", topic, true);
        m.reply_to = Some(anchor);
        store.put_message(&m, reply, true)?;
    }
    if label == ImplicitLabel::AnsweredElsewhere
        && let Some(sender) = senders.first()
    {
        store.record(&MetricEvent {
            chat: Some(*sender),
            ..MetricEvent::new(MetricKind::OutgoingDm, read_at.unwrap_or(last) + 120)
        })?;
    }
    store.resolve_ticket(id, label, read_at, reply_at, last + SECS_PER_DAY)?;
    if rng.chance(35) {
        let explicit = match rng.below(10) {
            0..=5 => ExplicitLabel::Want,
            6..=8 => ExplicitLabel::Noise,
            _ => ExplicitLabel::Skip,
        };
        store.label_ticket(id, Some(explicit), last + SECS_PER_DAY + 60)?;
    }
    Ok(true)
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
    topic: Option<MessageId>,
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
        reply_to: topic,
        album_id: None,
        mentioned: false,
        topic,
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
        let store = Store::open_in_memory().unwrap();
        let now = 1_760_000_000;
        let n = seed(&store, now).unwrap();
        assert!(n > 50, "only {n} tickets");
        assert!(seed(&store, now).is_err());
        let input = store.report_input(now - DAYS * SECS_PER_DAY, now).unwrap();
        let report = captain_inbox::report::build(&input, &Policy::default());
        assert!(!report.sections.is_empty());
    }
}
