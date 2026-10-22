//! Hand-computed fixtures for the dashboard metrics.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use captain_core::{ChatId, MessageId};
use captain_inbox::report::SELF_HANDLED;
use captain_inbox::{EntryKind, ImplicitLabel, ThreadKey, TicketState, TriggerKind};
use captain_store::{NewTicket, Store};
use hub_store::{
    ContentKind, ConversationId, ConversationKind, CountedMessage, Direction, Hub, LogLevel,
    ModuleHealth, ModuleState, Network, NewConversation, NewFailure, NewLogEvent,
};

use crate::{
    ConversationFilter, Source, conversations_page, daily_by_source, health, hourly_heatmap,
    overview, response_p50_by_day, telegram_summary, ticket_funnel, top_conversations,
    zalo_summary,
};

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

fn scratch_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let path = std::env::temp_dir().join(format!(
        "hub-analytics-test-{}-{nanos}-{n}",
        std::process::id()
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        Self(scratch_dir())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_seeded(seed: impl FnOnce(&Store)) -> (Tmp, Hub) {
    let tmp = Tmp::new();
    let path = tmp.0.join("inbox.sqlite");
    {
        let store = Store::open(&path).unwrap();
        seed(&store);
    }
    (tmp, Hub::open(&path).unwrap())
}

fn ticket(
    chat: i64,
    topic: Option<i32>,
    trigger: TriggerKind,
    state: TicketState,
    opened: i64,
    anchor: i64,
    reason: Option<&str>,
) -> NewTicket {
    NewTicket {
        key: ThreadKey {
            chat: ChatId(chat),
            topic: topic.map(MessageId),
        },
        chat_label: "ops".into(),
        chat_kind: Some(EntryKind::Group),
        profile: "group".into(),
        trigger,
        trigger_reason: "test".into(),
        route: None,
        state,
        state_reason: reason.map(str::to_owned),
        stale: false,
        note: None,
        opened_at: opened,
        closed_at: opened,
        anchor: MessageId(1),
        anchor_date: anchor,
        messages: vec![MessageId(1)],
        senders: vec![ChatId(2)],
    }
}

fn resolved(
    store: &Store,
    chat: i64,
    topic: Option<i32>,
    trigger: TriggerKind,
    at: i64,
    label: ImplicitLabel,
    reply_secs: Option<i64>,
) {
    let id = store
        .create_ticket(&ticket(
            chat,
            topic,
            trigger,
            TicketState::Queued,
            at,
            at,
            None,
        ))
        .unwrap();
    let reply = reply_secs.map(|secs| at + secs);
    store
        .resolve_ticket(id, label, None, reply, at + 1)
        .unwrap();
}

fn remember(
    hub: &mut Hub,
    network: Network,
    kind: ConversationKind,
    external_id: &str,
    title: &str,
    seen: i64,
    muted: bool,
) -> i64 {
    hub.upsert_conversation(&NewConversation {
        id: ConversationId {
            network,
            kind,
            external_id: external_id.to_owned(),
        },
        title: Some(title.to_owned()),
        topic_id: None,
        muted,
        tags: None,
        seen_at_unix_secs: seen,
    })
    .unwrap()
}

fn tally(
    hub: &mut Hub,
    external_id: &str,
    network: Network,
    kind: ConversationKind,
    at: i64,
    times: u64,
) {
    let conversation = ConversationId {
        network,
        kind,
        external_id: external_id.to_owned(),
    };
    for _ in 0..times {
        hub.record_message(&CountedMessage {
            conversation: conversation.clone(),
            at_unix_secs: at,
            direction: Direction::In,
            content_kind: ContentKind::Text,
        })
        .unwrap();
    }
}

#[test]
fn heatmap_cells_match_the_hand_offset() {
    let mut hub = Hub::open_in_memory().unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "c",
        "c",
        1,
        false,
    );
    // 1970-01-05 00:00 UTC is Monday. UTC+7 lands on Monday 07:00.
    tally(
        &mut hub,
        "c",
        Network::Telegram,
        ConversationKind::Group,
        345_600,
        1,
    );
    // 1970-01-03 20:00 UTC is Saturday. UTC+7 lands on Sunday 03:00.
    tally(
        &mut hub,
        "c",
        Network::Telegram,
        ConversationKind::Group,
        244_800,
        1,
    );
    let now = 4 * DAY + 10;
    let heat = hourly_heatmap(&hub.reader(), now, 3, Source::All, 420).unwrap();
    assert_eq!(heat.cells.len(), 7 * 24);
    assert_eq!(heat.offset_minutes, 420);
    assert_eq!(heat.cells[7].count, 1);
    assert_eq!(heat.cells[7].weekday, 0);
    assert_eq!(heat.cells[6 * 24 + 3].count, 1);
    assert_eq!(heat.cells.iter().map(|cell| cell.count).sum::<u64>(), 2);

    // The same Monday instant minus 60 minutes is Sunday 23:00.
    // Saturday 20:00 UTC minus 60 minutes is Saturday 19:00.
    let shifted = hourly_heatmap(&hub.reader(), now, 3, Source::All, -60).unwrap();
    assert_eq!(shifted.cells[6 * 24 + 23].count, 1);
    assert_eq!(shifted.cells[5 * 24 + 19].count, 1);
    assert_eq!(shifted.cells.iter().map(|cell| cell.count).sum::<u64>(), 2);

    let zalo = hourly_heatmap(&hub.reader(), now, 3, Source::Zalo, 420).unwrap();
    assert!(zalo.cells.iter().all(|cell| cell.count == 0));
}

#[test]
fn daily_totals_include_empty_networks() {
    let mut hub = Hub::open_in_memory().unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "t",
        "t",
        1,
        false,
    );
    remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Dm,
        "z",
        "z",
        1,
        false,
    );
    tally(
        &mut hub,
        "t",
        Network::Telegram,
        ConversationKind::Group,
        3_600,
        2,
    );
    tally(
        &mut hub,
        "z",
        Network::Zalo,
        ConversationKind::Dm,
        DAY + 10,
        1,
    );
    let rows = daily_by_source(&hub.reader(), DAY + 1, 2).unwrap();
    let count = |day, network| {
        rows.iter()
            .find(|row| row.day == day && row.source == network)
            .unwrap()
            .count
    };
    assert_eq!(rows.len(), 6);
    assert_eq!(count(0, Network::Telegram), 2);
    assert_eq!(count(0, Network::Zalo), 0);
    assert_eq!(count(0, Network::Messenger), 0);
    assert_eq!(count(1, Network::Telegram), 0);
    assert_eq!(count(1, Network::Zalo), 1);
    assert_eq!(count(1, Network::Messenger), 0);
}

#[test]
fn top_conversations_drop_zeros_and_break_ties_by_id() {
    let mut hub = Hub::open_in_memory().unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "a",
        "a",
        1,
        false,
    );
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "b",
        "b",
        1,
        false,
    );
    remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Dm,
        "z",
        "z",
        1,
        false,
    );
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Dm,
        "quiet",
        "quiet",
        1,
        false,
    );
    tally(
        &mut hub,
        "a",
        Network::Telegram,
        ConversationKind::Group,
        10,
        2,
    );
    tally(
        &mut hub,
        "b",
        Network::Telegram,
        ConversationKind::Group,
        10,
        2,
    );
    tally(&mut hub, "z", Network::Zalo, ConversationKind::Dm, 10, 5);
    let ranked = top_conversations(&hub.reader(), 100, 1, 10, Source::All).unwrap();
    assert_eq!(ranked.len(), 3);
    assert_eq!(ranked[0].external_id, "z");
    assert_eq!(ranked[0].messages, 5);
    assert!(ranked[1].id < ranked[2].id);
    assert_eq!(ranked[1].messages, 2);
    let only = top_conversations(&hub.reader(), 100, 1, 1, Source::Telegram).unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].messages, 2);
    assert!(
        top_conversations(&hub.reader(), 100, 1, 0, Source::All)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn response_median_uses_nearest_rank_and_flags_a_short_day() {
    let now = 10 * DAY + 100;
    let (_tmp, mut hub) = open_seeded(|store| {
        for secs in [60, 120, 180, 240] {
            resolved(
                store,
                42,
                None,
                TriggerKind::Mention,
                8 * DAY + 10,
                ImplicitLabel::Answered,
                Some(secs),
            );
        }
        for secs in [60, 120, 180, 240, 600] {
            resolved(
                store,
                42,
                None,
                TriggerKind::Mention,
                9 * DAY + 10,
                ImplicitLabel::Answered,
                Some(secs),
            );
        }
        resolved(
            store,
            -100_500,
            Some(77),
            TriggerKind::Mention,
            9 * DAY + 20,
            ImplicitLabel::Answered,
            Some(1),
        );
    });
    let zalo = remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Group,
        "g",
        "g",
        1,
        false,
    );
    hub.set_bridge_supergroup(-100_500).unwrap();
    hub.map_topic(zalo, 77, 1).unwrap();

    let days = response_p50_by_day(&hub.reader(), now, 3, Source::Telegram).unwrap();
    assert_eq!(days.len(), 3);
    assert_eq!(days[0].day, 8);
    assert_eq!(days[0].samples, 4);
    assert_eq!(days[0].p50_minutes, Some(2.0));
    assert!(days[0].low_sample);
    assert_eq!(days[1].day, 9);
    assert_eq!(days[1].samples, 5);
    assert_eq!(days[1].p50_minutes, Some(3.0));
    assert!(!days[1].low_sample);
    assert_eq!(days[2].day, 10);
    assert_eq!(days[2].samples, 0);
    assert_eq!(days[2].p50_minutes, None);
    assert!(days[2].low_sample);
}

#[test]
fn funnel_counts_match_the_412_split() {
    const NOW: i64 = 1_000_000;
    const INSIDE: i64 = 900_000;
    const OUTSIDE: i64 = -1_000_000;
    let (_tmp, hub) = open_seeded(|store| {
        for _ in 0..300 {
            resolved(
                store,
                42,
                None,
                TriggerKind::Mention,
                INSIDE,
                ImplicitLabel::Answered,
                Some(60),
            );
        }
        for _ in 0..50 {
            resolved(
                store,
                42,
                None,
                TriggerKind::Keyword,
                INSIDE,
                ImplicitLabel::AnsweredElsewhere,
                None,
            );
        }
        for _ in 0..40 {
            resolved(
                store,
                42,
                None,
                TriggerKind::ReplyToMe,
                INSIDE,
                ImplicitLabel::ReadOnly,
                None,
            );
        }
        for _ in 0..16 {
            resolved(
                store,
                42,
                None,
                TriggerKind::Manual,
                INSIDE,
                ImplicitLabel::Unseen,
                None,
            );
        }
        for _ in 0..6 {
            store
                .create_ticket(&ticket(
                    42,
                    None,
                    TriggerKind::Mention,
                    TicketState::Queued,
                    INSIDE,
                    INSIDE,
                    None,
                ))
                .unwrap();
        }
        for _ in 0..2 {
            store
                .create_ticket(&ticket(
                    42,
                    None,
                    TriggerKind::Mention,
                    TicketState::Digest,
                    INSIDE,
                    INSIDE,
                    None,
                ))
                .unwrap();
        }
        for _ in 0..3 {
            store
                .create_ticket(&ticket(
                    42,
                    None,
                    TriggerKind::Mention,
                    TicketState::Skipped,
                    INSIDE,
                    INSIDE,
                    None,
                ))
                .unwrap();
        }
        store
            .create_ticket(&ticket(
                42,
                None,
                TriggerKind::Mention,
                TicketState::Queued,
                OUTSIDE,
                OUTSIDE,
                None,
            ))
            .unwrap();
    });
    let funnel = ticket_funnel(&hub.reader(), NOW, 14, Source::Telegram).unwrap();
    assert_eq!(funnel.queued, 6);
    assert_eq!(funnel.digest, 2);
    assert_eq!(funnel.resolved, 406);
    assert_eq!(funnel.queued + funnel.resolved, 412);
    assert_eq!(funnel.resolved_labels.answered, 300);
    assert_eq!(funnel.resolved_labels.answered_elsewhere, 50);
    assert_eq!(funnel.resolved_labels.read_only, 40);
    assert_eq!(funnel.resolved_labels.unseen, 16);
    assert_eq!(funnel.resolved_labels.unlabelled, 0);
    let labels = funnel.resolved_labels;
    assert_eq!(
        labels.answered
            + labels.answered_elsewhere
            + labels.read_only
            + labels.unseen
            + labels.unlabelled,
        funnel.resolved
    );
    let json = serde_json::to_value(funnel).unwrap();
    assert_eq!(json["draft"], "not_available_in_p1");

    let cards = overview(&hub.reader(), NOW, 14, Source::Telegram).unwrap();
    assert_eq!(cards.queued_tickets, 7);
    assert_eq!(cards.digest_tickets, 2);
    assert_eq!(cards.response_samples, 300);
    assert_eq!(cards.response_p50_minutes, Some(1.0));
    let value = serde_json::to_value(&cards).unwrap();
    for key in [
        "messages_24h",
        "queued_tickets",
        "digest_tickets",
        "response_p50_minutes",
        "response_samples",
        "active_conversations",
        "watched_conversations",
        "modules",
    ] {
        assert!(value.get(key).is_some(), "{key}");
    }
}

#[test]
fn funnel_window_is_half_open() {
    const NOW: i64 = 1_000_000;
    let start = NOW - DAY;
    let (_tmp, hub) = open_seeded(|store| {
        for opened in [start, start - 1, NOW, NOW - 1] {
            store
                .create_ticket(&ticket(
                    42,
                    None,
                    TriggerKind::Mention,
                    TicketState::Queued,
                    opened,
                    opened,
                    None,
                ))
                .unwrap();
        }
    });
    let funnel = ticket_funnel(&hub.reader(), NOW, 1, Source::All).unwrap();
    assert_eq!(funnel.queued, 2);
}

#[test]
fn overview_24h_splits_sources_and_ignores_the_older_hour() {
    let now = 10 * DAY + 12 * HOUR + 100;
    let mut hub = Hub::open_in_memory().unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "t",
        "t",
        now - 10,
        false,
    );
    remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Dm,
        "z",
        "z",
        now - 2 * DAY,
        true,
    );
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Dm,
        "old",
        "old",
        now - 2 * DAY,
        false,
    );
    tally(
        &mut hub,
        "t",
        Network::Telegram,
        ConversationKind::Group,
        now - 10,
        1,
    );
    tally(
        &mut hub,
        "z",
        Network::Zalo,
        ConversationKind::Dm,
        now - 23 * HOUR,
        1,
    );
    tally(
        &mut hub,
        "t",
        Network::Telegram,
        ConversationKind::Group,
        now - 24 * HOUR,
        1,
    );

    let all = overview(&hub.reader(), now, 1, Source::All).unwrap();
    assert_eq!(all.messages_24h.len(), 3);
    assert_eq!(all.messages_24h[0].count, 1);
    assert_eq!(all.messages_24h[1].count, 1);
    assert_eq!(all.messages_24h[2].count, 0);
    assert_eq!(all.active_conversations, 1);
    assert_eq!(all.watched_conversations, 2);
    let telegram = overview(&hub.reader(), now, 1, Source::Telegram).unwrap();
    assert_eq!(telegram.messages_24h.len(), 1);
    assert_eq!(telegram.messages_24h[0].source, Network::Telegram);
    assert_eq!(telegram.messages_24h[0].count, 1);
}

#[test]
fn conversations_page_matches_title_sparkline_and_open_tickets() {
    let now = 100 * DAY + 50;
    let (_tmp, mut hub) = open_seeded(|store| {
        store
            .create_ticket(&ticket(
                -100,
                None,
                TriggerKind::Mention,
                TicketState::Queued,
                now - 5,
                now - 5,
                None,
            ))
            .unwrap();
        store
            .create_ticket(&ticket(
                -100_500,
                Some(77),
                TriggerKind::Mention,
                TicketState::Queued,
                now - 5,
                now - 5,
                None,
            ))
            .unwrap();
    });
    hub.set_bridge_supergroup(-100_500).unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "-100",
        "Đội ops",
        now - 10,
        false,
    );
    let zalo = remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Dm,
        "z1",
        "z1",
        now - 20,
        true,
    );
    hub.map_topic(zalo, 77, now - 20).unwrap();
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "-100500",
        "bridge",
        now - 30,
        false,
    );
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "old",
        "đội cũ",
        now - 40 * DAY,
        false,
    );
    tally(
        &mut hub,
        "-100",
        Network::Telegram,
        ConversationKind::Group,
        100 * DAY + 10,
        3,
    );
    tally(
        &mut hub,
        "-100",
        Network::Telegram,
        ConversationKind::Group,
        99 * DAY + 10,
        1,
    );
    tally(
        &mut hub,
        "-100",
        Network::Telegram,
        ConversationKind::Group,
        94 * DAY + 10,
        2,
    );
    tally(
        &mut hub,
        "-100",
        Network::Telegram,
        ConversationKind::Group,
        93 * DAY + 10,
        5,
    );

    let page = conversations_page(
        &hub.reader(),
        now,
        &ConversationFilter {
            source: Source::All,
            kind: None,
            days: None,
            query: None,
        },
        10,
        0,
    )
    .unwrap();
    assert_eq!(page.total, 4);
    assert_eq!(
        page.items
            .iter()
            .map(|item| item.external_id.as_str())
            .collect::<Vec<_>>(),
        vec!["-100", "z1", "-100500", "old"]
    );
    assert_eq!(page.items[0].sparkline, vec![2, 0, 0, 0, 0, 1, 3]);
    assert_eq!(page.items[0].open_tickets, 1);
    assert_eq!(page.items[1].open_tickets, 1);
    assert!(page.items[1].muted);
    assert_eq!(page.items[2].open_tickets, 0);
    assert_eq!(page.items[3].open_tickets, 0);

    let second = conversations_page(
        &hub.reader(),
        now,
        &ConversationFilter {
            source: Source::All,
            kind: None,
            days: None,
            query: None,
        },
        1,
        1,
    )
    .unwrap();
    assert_eq!(second.total, 4);
    assert_eq!(second.items[0].external_id, "z1");
    assert!(
        conversations_page(
            &hub.reader(),
            now,
            &ConversationFilter {
                source: Source::All,
                kind: None,
                days: None,
                query: None,
            },
            0,
            0,
        )
        .unwrap()
        .items
        .is_empty()
    );

    let query = conversations_page(
        &hub.reader(),
        now,
        &ConversationFilter {
            source: Source::All,
            kind: None,
            days: Some(7),
            query: Some("ĐỘI".into()),
        },
        10,
        0,
    )
    .unwrap();
    assert_eq!(query.total, 1);
    assert_eq!(query.items[0].external_id, "-100");
}

#[test]
fn telegram_summary_ignores_a_zalo_bridge_ticket() {
    let now = 5 * DAY;
    let (_tmp, mut hub) = open_seeded(|store| {
        resolved(
            store,
            42,
            None,
            TriggerKind::Mention,
            now - 100,
            ImplicitLabel::Unseen,
            None,
        );
        resolved(
            store,
            -100_500,
            Some(77),
            TriggerKind::Mention,
            now - 80,
            ImplicitLabel::Answered,
            Some(30),
        );
        store
            .create_ticket(&ticket(
                42,
                None,
                TriggerKind::Keyword,
                TicketState::Skipped,
                now - 50,
                now - 50,
                Some(SELF_HANDLED),
            ))
            .unwrap();
    });
    let zalo = remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Group,
        "g",
        "g",
        1,
        false,
    );
    hub.set_bridge_supergroup(-100_500).unwrap();
    hub.map_topic(zalo, 77, 1).unwrap();
    let summary = telegram_summary(&hub.reader(), now, 1).unwrap();
    assert_eq!(summary.tickets, 1);
    assert_eq!(summary.labelled, 1);
    assert_eq!(summary.resolved_ratio, Some(0.0));
    assert_eq!(summary.triggers.len(), 1);
    assert_eq!(summary.triggers[0].trigger, "mention");
    assert_eq!(summary.triggers[0].tickets, 1);
    assert_eq!(summary.triggers[0].answered_ratio, Some(0.0));
}

#[test]
fn zalo_summary_counts_kinds_and_unresolved_zalo_failures() {
    let now = 20 * DAY;
    let mut hub = Hub::open_in_memory().unwrap();
    remember(
        &mut hub,
        Network::Zalo,
        ConversationKind::Group,
        "z",
        "z",
        1,
        false,
    );
    remember(
        &mut hub,
        Network::Telegram,
        ConversationKind::Group,
        "t",
        "t",
        1,
        false,
    );
    let zalo = ConversationId {
        network: Network::Zalo,
        kind: ConversationKind::Group,
        external_id: "z".into(),
    };
    let telegram = ConversationId {
        network: Network::Telegram,
        kind: ConversationKind::Group,
        external_id: "t".into(),
    };
    for _ in 0..2 {
        hub.record_message(&CountedMessage {
            conversation: zalo.clone(),
            at_unix_secs: now - DAY,
            direction: Direction::Out,
            content_kind: ContentKind::Text,
        })
        .unwrap();
    }
    hub.record_message(&CountedMessage {
        conversation: zalo,
        at_unix_secs: now - DAY,
        direction: Direction::In,
        content_kind: ContentKind::Photo,
    })
    .unwrap();
    hub.record_message(&CountedMessage {
        conversation: telegram,
        at_unix_secs: now - DAY,
        direction: Direction::In,
        content_kind: ContentKind::Text,
    })
    .unwrap();
    let zalo_id = hub
        .reader()
        .conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.external_id == "z")
        .unwrap()
        .id;
    let telegram_id = hub
        .reader()
        .conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.external_id == "t")
        .unwrap()
        .id;
    hub.record_failure(&NewFailure {
        conversation_id: Some(zalo_id),
        reason: "zalo".into(),
        retryable: true,
        created_at_unix_secs: 1,
    })
    .unwrap();
    hub.record_failure(&NewFailure {
        conversation_id: None,
        reason: "bridge".into(),
        retryable: false,
        created_at_unix_secs: 1,
    })
    .unwrap();
    hub.record_failure(&NewFailure {
        conversation_id: Some(telegram_id),
        reason: "telegram".into(),
        retryable: true,
        created_at_unix_secs: 1,
    })
    .unwrap();
    let resolved = hub
        .record_failure(&NewFailure {
            conversation_id: Some(zalo_id),
            reason: "done".into(),
            retryable: true,
            created_at_unix_secs: 1,
        })
        .unwrap();
    hub.resolve_failure(resolved, 2).unwrap();

    let summary = zalo_summary(&hub.reader(), now, 4).unwrap();
    assert_eq!(summary.window_days, 14);
    assert_eq!(summary.topicless_groups, 4);
    assert_eq!(summary.failed_events, 2);
    assert_eq!(summary.kinds.len(), ContentKind::ALL.len());
    let kind = |wanted| {
        summary
            .kinds
            .iter()
            .find(|row| row.kind == wanted)
            .unwrap()
            .count
    };
    assert_eq!(kind(ContentKind::Text), 2);
    assert_eq!(kind(ContentKind::Photo), 1);
    assert_eq!(kind(ContentKind::Video), 0);
    assert_eq!(kind(ContentKind::BankCard), 0);
}

#[test]
fn health_fills_missing_levels_with_zero() {
    let now = 50_000;
    let mut hub = Hub::open_in_memory().unwrap();
    hub.record_health(&ModuleHealth {
        module: "zalo".into(),
        state: ModuleState::Degraded,
        detail: "slow".into(),
        updated_at_unix_secs: now - 10,
    })
    .unwrap();
    hub.record_failure(&NewFailure {
        conversation_id: None,
        reason: "retry".into(),
        retryable: true,
        created_at_unix_secs: 1,
    })
    .unwrap();
    hub.record_failure(&NewFailure {
        conversation_id: None,
        reason: "drop".into(),
        retryable: false,
        created_at_unix_secs: 1,
    })
    .unwrap();
    let resolved = hub
        .record_failure(&NewFailure {
            conversation_id: None,
            reason: "old".into(),
            retryable: true,
            created_at_unix_secs: 1,
        })
        .unwrap();
    hub.resolve_failure(resolved, 2).unwrap();
    hub.record_bridge_event(&NewLogEvent {
        at_unix_secs: now - 10,
        module: "bridge".into(),
        level: LogLevel::Warn,
        kind: "send".into(),
        detail: None,
    })
    .unwrap();
    hub.record_bridge_event(&NewLogEvent {
        at_unix_secs: now - DAY - 1,
        module: "bridge".into(),
        level: LogLevel::Info,
        kind: "old".into(),
        detail: None,
    })
    .unwrap();
    hub.record_module_event(&NewLogEvent {
        at_unix_secs: now - 5,
        module: "zalo".into(),
        level: LogLevel::Error,
        kind: "down".into(),
        detail: None,
    })
    .unwrap();
    let page = health(&hub.reader(), now).unwrap();
    assert_eq!(page.modules.len(), 1);
    assert_eq!(page.modules[0].state, ModuleState::Degraded);
    assert_eq!(page.unresolved_failures, 2);
    assert_eq!(page.retryable_failures, 1);
    assert_eq!(page.bridge_events_24h.len(), 4);
    assert_eq!(
        page.bridge_events_24h
            .iter()
            .map(|row| row.count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        page.bridge_events_24h
            .iter()
            .find(|row| row.level == LogLevel::Warn)
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        page.module_events_24h
            .iter()
            .find(|row| row.level == LogLevel::Error)
            .unwrap()
            .count,
        1
    );
    assert_eq!(
        page.module_events_24h
            .iter()
            .map(|row| row.count)
            .sum::<u64>(),
        1
    );
}
