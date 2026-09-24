//! The phase 1 report: metrics M1 to M19 and the decision rules in
//! `docs/design/metrics-p1.md`, computed from facts the store extracts.
//!
//! Nothing here contains message text. Inputs are ids, counts, times and labels.

use std::collections::{BTreeMap, HashMap};

use captain_core::ChatId;
use serde::Serialize;

use crate::config::{EntryKind, Policy};
use crate::stats::{mean, percentile, ratio};
use crate::ticket::{ExplicitLabel, ImplicitLabel, Route, ThreadKey, TicketState};
use crate::trigger::TriggerKind;
use crate::unix_day;

const SECS_PER_DAY: i64 = 86_400;
const MIN_SAMPLE: usize = 10; // below this, rules report "not enough data" instead of firing
const REOPEN_WINDOW_SECS: i64 = 600; // M8: a ticket this soon after the last one on a key
const DRAFTS_PER_DAY_MAX: f64 = 40.0;
const DRAFTS_PER_DAY_MIN: f64 = 3.0;
const IMPLICIT_PRECISION_MIN: f64 = 0.3;
const EXPLICIT_PRECISION_MIN: f64 = 0.5;
const RECALL_MIN: f64 = 0.8;
const REOPEN_RATE_MAX: f64 = 0.1;
const DEBOUNCE_MARGIN_SECS: i64 = 5;
const DEBOUNCE_FLOOR_SECS: i64 = 15;
const DEBOUNCE_CEILING_SECS: i64 = 120;
const BUDGET_HEADROOM: f64 = 1.5;
const UPTIME_MIN: f64 = 0.97;
const FLOOD_WAIT_MAX_SECS: i64 = 60;
const CANDIDATES_SHOWN: usize = 10;
const BYTES_PER_MB: f64 = 1_048_576.0;

/// One ticket, reduced to what the metrics use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketFact {
    /// Ticket id.
    pub id: i64,
    /// Thread.
    pub key: ThreadKey,
    /// Scope label of the chat.
    pub chat_label: String,
    /// Kind of the chat's scope entry; `None` for manual tickets on unscoped chats.
    pub chat_kind: Option<EntryKind>,
    /// Profile of the burst.
    pub profile: String,
    /// First trigger.
    pub trigger: TriggerKind,
    /// Route, if any.
    pub route: Option<Route>,
    /// State now.
    pub state: TicketState,
    /// Why it was skipped or sent to the digest.
    pub reason: Option<String>,
    /// When the burst opened.
    pub opened_at: i64,
    /// When the burst closed.
    pub closed_at: i64,
    /// Send time of the anchor.
    pub anchor_date: i64,
    /// Messages in the burst.
    pub messages: usize,
    /// Label from behaviour, once resolved.
    pub implicit: Option<ImplicitLabel>,
    /// Label from review.
    pub explicit: Option<ExplicitLabel>,
    /// Anchor to first read covering it.
    pub read_secs: Option<i64>,
    /// Anchor to the owner's first message on the key.
    pub reply_secs: Option<i64>,
}

/// Incoming scoped messages of one chat on one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundCount {
    /// Chat.
    pub chat: ChatId,
    /// Scope label.
    pub label: String,
    /// Day number since the epoch.
    pub day: i64,
    /// Messages.
    pub count: usize,
}

/// Reply chain shape of a ticket's anchor, from `context_shape` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextShape {
    /// Reply hops from the anchor to the start of its chain.
    pub depth: i64,
    /// Age of the oldest message in that chain, relative to the anchor.
    pub oldest_age_secs: i64,
    /// Messages on the key in the hour before the anchor.
    pub prior_hour: i64,
    /// Messages on the key in the day before the anchor.
    pub prior_day: i64,
}

/// A message from someone else that the owner answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answered {
    /// Whether some ticket includes it.
    pub covered: bool,
}

/// One daemon run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSpan {
    /// Start time.
    pub started_at: i64,
    /// Last heartbeat or stop time.
    pub stopped_at: i64,
}

/// Kinds of API trouble counted in M17.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiTrouble {
    /// `FLOOD_WAIT_X`; the value is X.
    FloodWait,
    /// Any other RPC error.
    RpcError,
}

/// One API error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEvent {
    /// What happened.
    pub kind: ApiTrouble,
    /// Request name.
    pub method: String,
    /// Seconds for flood waits, the error code otherwise.
    pub value: i64,
}

/// An out-of-scope chat that would have triggered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// Chat.
    pub chat: ChatId,
    /// Title, when known.
    pub title: Option<String>,
    /// Messages that would have fired a mention or private-chat trigger.
    pub would_trigger: usize,
    /// All messages counted.
    pub messages: usize,
}

/// Everything the report needs, for the window `since..until`.
#[derive(Debug, Clone, Default)]
pub struct ReportInput {
    /// Window start.
    pub since: i64,
    /// Window end.
    pub until: i64,
    /// Tickets opened in the window.
    pub tickets: Vec<TicketFact>,
    /// Scoped inbound counts.
    pub inbound: Vec<InboundCount>,
    /// Gaps between consecutive incoming messages on a thread, within five minutes, by
    /// profile name.
    pub burst_gaps: Vec<(String, i64)>,
    /// Messages the owner answered.
    pub answered: Vec<Answered>,
    /// Context shape per closed ticket.
    pub shapes: Vec<ContextShape>,
    /// Daemon runs overlapping the window.
    pub runs: Vec<RunSpan>,
    /// Receive time minus send time, for live messages.
    pub update_lags_secs: Vec<i64>,
    /// API errors.
    pub api: Vec<ApiEvent>,
    /// Daily store size samples, `(unix time, bytes)`.
    pub store_sizes: Vec<(i64, i64)>,
    /// Out-of-scope chats that would have triggered.
    pub candidates: Vec<Candidate>,
}

/// Unit of a [`Stat`] value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// A plain count.
    Count,
    /// Per day.
    PerDay,
    /// A fraction from 0 to 1.
    Ratio,
    /// Seconds.
    Seconds,
    /// Megabytes.
    Megabytes,
    /// Megabytes per day.
    MegabytesPerDay,
}

/// One number of a metric.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stat {
    /// For example `p50`.
    pub label: String,
    /// `None` when there is no data.
    pub value: Option<f64>,
    /// Unit.
    pub unit: Unit,
}

/// A metric broken down by one key, such as trigger kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Breakdown {
    /// For example `mention`.
    pub key: String,
    /// Numbers for that key.
    pub stats: Vec<Stat>,
    /// Sample size behind them.
    pub n: usize,
}

/// One metric.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metric {
    /// `M1` to `M19`.
    pub id: &'static str,
    /// Short name.
    pub name: &'static str,
    /// Headline numbers.
    pub summary: Vec<Stat>,
    /// Per-key numbers, possibly empty.
    pub breakdown: Vec<Breakdown>,
    /// Sample size behind the headline.
    pub n: usize,
}

/// Metrics grouped under one question.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    /// `Q1` to `Q7`.
    pub question: &'static str,
    /// The question.
    pub title: &'static str,
    /// Its metrics.
    pub metrics: Vec<Metric>,
}

/// State of one decision rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    /// The condition holds; act on it.
    Fires,
    /// The condition does not hold.
    Holds,
    /// Too little data to tell.
    NotEnoughData,
    /// Always applies; the detail is a suggested value.
    Suggestion,
}

/// One decision rule, evaluated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rule {
    /// Short id, for example `volume_high`.
    pub id: &'static str,
    /// State.
    pub state: RuleState,
    /// What the rule checks.
    pub when: &'static str,
    /// What to do when it fires, or the suggested value.
    pub then: String,
}

/// Tickets on one day, by route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DailyTickets {
    /// Day number since the epoch.
    pub day: i64,
    /// Routed to a draft.
    pub draft: usize,
    /// Routed to the digest.
    pub digest: usize,
    /// Skipped.
    pub skipped: usize,
}

/// The phase 1 report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Window start.
    pub since: i64,
    /// Window end.
    pub until: i64,
    /// Window length in days.
    pub days: f64,
    /// Metrics by question.
    pub sections: Vec<Section>,
    /// Decision rules.
    pub rules: Vec<Rule>,
    /// Tickets per day, for the volume chart.
    pub daily: Vec<DailyTickets>,
    /// Top out-of-scope chats that would have triggered.
    pub candidates: Vec<Candidate>,
}

/// Computes the report for `input` under `policy`.
#[expect(
    clippy::cast_precision_loss,
    reason = "counts and seconds stay far below 2^52"
)]
pub fn build(input: &ReportInput, policy: &Policy) -> Report {
    let days = ((input.until - input.since) as f64 / SECS_PER_DAY as f64).max(1.0 / 24.0);
    let per_day = |n: usize| Some(n as f64 / days);
    let mut ctx = Ctx {
        input,
        policy,
        days,
        rules: Vec::new(),
    };

    let q1 = Section {
        question: "Q1",
        title: "How much arrives?",
        metrics: vec![ctx.m1(per_day), ctx.m2(per_day), ctx.m3(), ctx.m19()],
    };
    let q2 = Section {
        question: "Q2",
        title: "Which triggers point at messages you answer?",
        metrics: vec![ctx.m4(), ctx.m5()],
    };
    let q3 = Section {
        question: "Q3",
        title: "What did no trigger catch?",
        metrics: vec![ctx.m6()],
    };
    let q4 = Section {
        question: "Q4",
        title: "What do bursts look like?",
        metrics: vec![ctx.m7(), ctx.m8(), ctx.m9()],
    };
    let q5 = Section {
        question: "Q5",
        title: "How much history does a triggered message depend on?",
        metrics: vec![ctx.m10(), ctx.m11()],
    };
    let q6 = Section {
        question: "Q6",
        title: "How fast do you read and reply today?",
        metrics: vec![ctx.m12(), ctx.m13()],
    };
    let q7 = Section {
        question: "Q7",
        title: "Is the daemon reliable?",
        metrics: vec![ctx.m14(), ctx.m15(), ctx.m16(), ctx.m17(), ctx.m18()],
    };

    Report {
        since: input.since,
        until: input.until,
        days,
        sections: vec![q1, q2, q3, q4, q5, q6, q7],
        rules: ctx.rules,
        daily: daily(input),
        candidates: top_candidates(&input.candidates),
    }
}

struct Ctx<'a> {
    input: &'a ReportInput,
    policy: &'a Policy,
    days: f64,
    rules: Vec<Rule>,
}

fn stat(label: impl Into<String>, value: Option<f64>, unit: Unit) -> Stat {
    Stat {
        label: label.into(),
        value,
        unit,
    }
}

#[expect(clippy::cast_precision_loss, reason = "seconds stay far below 2^52")]
fn pct(values: &[i64], p: u8, unit: Unit) -> Stat {
    stat(
        format!("p{p}"),
        percentile(values, p).map(|v| v as f64),
        unit,
    )
}

fn pcts(values: &[i64], ps: &[u8], unit: Unit) -> Vec<Stat> {
    ps.iter().map(|p| pct(values, *p, unit)).collect()
}

fn source(kind: Option<EntryKind>) -> &'static str {
    match kind {
        Some(EntryKind::Dm) => "dm",
        Some(EntryKind::Group | EntryKind::Forum) => "group",
        None => "manual",
    }
}

impl<'a> Ctx<'a> {
    fn tickets(&self) -> impl Iterator<Item = &'a TicketFact> + use<'a> {
        self.input.tickets.iter()
    }

    /// Tickets that were not cancelled because the owner was already writing.
    fn real_tickets(&self) -> impl Iterator<Item = &'a TicketFact> + use<'a> {
        self.tickets()
            .filter(|t| t.reason.as_deref() != Some(SELF_HANDLED))
    }

    fn rule(&mut self, id: &'static str, state: RuleState, when: &'static str, then: String) {
        self.rules.push(Rule {
            id,
            state,
            when,
            then,
        });
    }

    fn m1(&self, per_day: impl Fn(usize) -> Option<f64>) -> Metric {
        let mut by_chat: BTreeMap<&str, usize> = BTreeMap::new();
        for c in &self.input.inbound {
            *by_chat.entry(c.label.as_str()).or_default() += c.count;
        }
        let total: usize = by_chat.values().sum();
        Metric {
            id: "M1",
            name: "Scoped inbound messages",
            summary: vec![stat("per day", per_day(total), Unit::PerDay)],
            breakdown: by_chat
                .into_iter()
                .map(|(label, n)| Breakdown {
                    key: label.to_owned(),
                    stats: vec![stat("per day", per_day(n), Unit::PerDay)],
                    n,
                })
                .collect(),
            n: total,
        }
    }

    fn m2(&mut self, per_day: impl Fn(usize) -> Option<f64>) -> Metric {
        let real: Vec<&TicketFact> = self.real_tickets().collect();
        let drafts = real
            .iter()
            .filter(|t| t.route == Some(Route::Draft))
            .count();
        let mut by_trigger: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for t in &real {
            let e = by_trigger.entry(t.trigger.as_str()).or_default();
            e.0 += 1;
            if t.route == Some(Route::Draft) {
                e.1 += 1;
            }
        }
        let drafts_per_day = per_day(drafts).unwrap_or_default();
        let enough = real.len() >= MIN_SAMPLE;
        let state = |fires: bool| match (enough, fires) {
            (false, _) => RuleState::NotEnoughData,
            (true, true) => RuleState::Fires,
            (true, false) => RuleState::Holds,
        };
        self.rule(
            "volume_high",
            state(drafts_per_day > DRAFTS_PER_DAY_MAX),
            "more than 40 draft-routed tickets a day",
            "Tighten scope before P2: review fatigue and cost grow with volume.".into(),
        );
        self.rule(
            "volume_low",
            state(drafts_per_day < DRAFTS_PER_DAY_MIN),
            "fewer than 3 draft-routed tickets a day",
            "P2 saves little. Widen scope with the candidate chats first.".into(),
        );
        Metric {
            id: "M2",
            name: "Tickets",
            summary: vec![
                stat("per day", per_day(real.len()), Unit::PerDay),
                stat("draft-routed per day", per_day(drafts), Unit::PerDay),
            ],
            breakdown: by_trigger
                .into_iter()
                .map(|(k, (n, d))| Breakdown {
                    key: k.to_owned(),
                    stats: vec![
                        stat("per day", per_day(n), Unit::PerDay),
                        stat("draft-routed per day", per_day(d), Unit::PerDay),
                    ],
                    n,
                })
                .collect(),
            n: real.len(),
        }
    }

    fn m3(&self) -> Metric {
        let sizes: Vec<i64> = self
            .real_tickets()
            .map(|t| i64::try_from(t.messages).unwrap_or(i64::MAX))
            .collect();
        let mut summary = vec![stat("mean", mean(&sizes), Unit::Count)];
        summary.extend(pcts(&sizes, &[50, 90], Unit::Count));
        Metric {
            id: "M3",
            name: "Messages per ticket",
            summary,
            breakdown: Vec::new(),
            n: sizes.len(),
        }
    }

    fn m4(&mut self) -> Metric {
        let mut by_trigger: BTreeMap<TriggerKind, (usize, usize)> = BTreeMap::new();
        for t in self.real_tickets() {
            if let Some(label) = t.implicit {
                let e = by_trigger.entry(t.trigger).or_default();
                e.1 += 1;
                if label.is_answer() {
                    e.0 += 1;
                }
            }
        }
        let explicit = self.explicit_by_trigger();
        for (kind, (hit, n)) in &by_trigger {
            let implicit = ratio(*hit, *n);
            let (want, labelled) = explicit.get(kind).copied().unwrap_or_default();
            let explicit_ratio = ratio(want, labelled);
            let state = if *n < MIN_SAMPLE {
                RuleState::NotEnoughData
            } else if implicit.is_some_and(|r| r < IMPLICIT_PRECISION_MIN)
                && explicit_ratio.is_none_or(|r| r < EXPLICIT_PRECISION_MIN)
            {
                RuleState::Fires
            } else {
                RuleState::Holds
            };
            self.rule(
                "trigger_precision",
                state,
                "a trigger kind has implicit precision under 30 % and explicit under 50 %",
                format!("Route {kind} to the digest instead of drafts."),
            );
        }
        let (hit, n) = by_trigger
            .values()
            .fold((0, 0), |(h, n), (a, b)| (h + a, n + b));
        Metric {
            id: "M4",
            name: "Implicit precision (answered)",
            summary: vec![stat("all", ratio(hit, n), Unit::Ratio)],
            breakdown: by_trigger
                .into_iter()
                .map(|(k, (h, n))| Breakdown {
                    key: k.as_str().to_owned(),
                    stats: vec![stat("answered", ratio(h, n), Unit::Ratio)],
                    n,
                })
                .collect(),
            n,
        }
    }

    fn explicit_by_trigger(&self) -> BTreeMap<TriggerKind, (usize, usize)> {
        let mut by_trigger: BTreeMap<TriggerKind, (usize, usize)> = BTreeMap::new();
        for t in self.real_tickets() {
            match t.explicit {
                Some(ExplicitLabel::Want) => {
                    let e = by_trigger.entry(t.trigger).or_default();
                    e.0 += 1;
                    e.1 += 1;
                }
                Some(ExplicitLabel::Noise) => by_trigger.entry(t.trigger).or_default().1 += 1,
                Some(ExplicitLabel::Skip) | None => {}
            }
        }
        by_trigger
    }

    fn m5(&self) -> Metric {
        let by_trigger = self.explicit_by_trigger();
        let (hit, n) = by_trigger
            .values()
            .fold((0, 0), |(h, n), (a, b)| (h + a, n + b));
        Metric {
            id: "M5",
            name: "Explicit precision (would want a draft)",
            summary: vec![stat("all", ratio(hit, n), Unit::Ratio)],
            breakdown: by_trigger
                .into_iter()
                .map(|(k, (h, n))| Breakdown {
                    key: k.as_str().to_owned(),
                    stats: vec![stat("want", ratio(h, n), Unit::Ratio)],
                    n,
                })
                .collect(),
            n,
        }
    }

    fn m6(&mut self) -> Metric {
        let n = self.input.answered.len();
        let covered = self.input.answered.iter().filter(|a| a.covered).count();
        let recall = ratio(covered, n);
        let state = if n < MIN_SAMPLE {
            RuleState::NotEnoughData
        } else if recall.is_some_and(|r| r < RECALL_MIN) {
            RuleState::Fires
        } else {
            RuleState::Holds
        };
        self.rule(
            "recall_low",
            state,
            "under 80 % of the messages you answered had a ticket",
            "Look at the missed messages and add or fix a trigger before P2.".into(),
        );
        Metric {
            id: "M6",
            name: "Recall",
            summary: vec![stat("covered", recall, Unit::Ratio)],
            breakdown: Vec::new(),
            n,
        }
    }

    fn m7(&mut self) -> Metric {
        let mut by_profile: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
        for (profile, gap) in &self.input.burst_gaps {
            by_profile.entry(profile.as_str()).or_default().push(*gap);
        }
        for (profile, gaps) in &by_profile {
            let current = self.policy.profile(profile).debounce_secs;
            let (state, then) = match percentile(gaps, 90) {
                Some(p90) if gaps.len() >= MIN_SAMPLE => {
                    let suggested = (p90 + DEBOUNCE_MARGIN_SECS)
                        .clamp(DEBOUNCE_FLOOR_SECS, DEBOUNCE_CEILING_SECS);
                    (
                        RuleState::Suggestion,
                        format!("{profile}: debounce_secs = {suggested} (now {current})"),
                    )
                }
                _ => (
                    RuleState::NotEnoughData,
                    format!("{profile}: keep debounce_secs = {current}"),
                ),
            };
            self.rule(
                "debounce",
                state,
                "debounce_secs = p90 of burst gaps plus 5 s, within 15 s to 120 s",
                then,
            );
        }
        let all: Vec<i64> = self.input.burst_gaps.iter().map(|(_, g)| *g).collect();
        Metric {
            id: "M7",
            name: "Gaps inside bursts",
            summary: pcts(&all, &[50, 90, 95], Unit::Seconds),
            breakdown: by_profile
                .into_iter()
                .map(|(k, v)| Breakdown {
                    key: k.to_owned(),
                    stats: pcts(&v, &[50, 90, 95], Unit::Seconds),
                    n: v.len(),
                })
                .collect(),
            n: all.len(),
        }
    }

    fn m8(&mut self) -> Metric {
        let mut last_close: HashMap<ThreadKey, i64> = HashMap::new();
        let mut by_profile: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        let mut ordered: Vec<&TicketFact> = self.real_tickets().collect();
        ordered.sort_by_key(|t| t.opened_at);
        for t in ordered {
            let reopened = last_close
                .get(&t.key)
                .is_some_and(|closed| t.opened_at - closed <= REOPEN_WINDOW_SECS);
            last_close.insert(t.key, t.closed_at);
            let e = by_profile.entry(t.profile.as_str()).or_default();
            e.1 += 1;
            if reopened {
                e.0 += 1;
            }
        }
        for (profile, (reopened, n)) in &by_profile {
            let rate = ratio(*reopened, *n);
            let state = if *n < MIN_SAMPLE {
                RuleState::NotEnoughData
            } else if rate.is_some_and(|r| r > REOPEN_RATE_MAX) {
                RuleState::Fires
            } else {
                RuleState::Holds
            };
            self.rule(
                "reopen_high",
                state,
                "over 10 % of tickets reopen within 10 minutes of the previous one",
                format!("Raise debounce_secs for profile {profile}."),
            );
        }
        let (reopened, n) = by_profile
            .values()
            .fold((0, 0), |(r, n), (a, b)| (r + a, n + b));
        Metric {
            id: "M8",
            name: "Reopen rate",
            summary: vec![stat("all", ratio(reopened, n), Unit::Ratio)],
            breakdown: by_profile
                .into_iter()
                .map(|(k, (r, n))| Breakdown {
                    key: k.to_owned(),
                    stats: vec![stat("reopened", ratio(r, n), Unit::Ratio)],
                    n,
                })
                .collect(),
            n,
        }
    }

    fn m9(&self) -> Metric {
        let all = self.tickets().count();
        let handled = self
            .tickets()
            .filter(|t| t.reason.as_deref() == Some(SELF_HANDLED))
            .count();
        Metric {
            id: "M9",
            name: "Self-handled bursts",
            summary: vec![stat("share", ratio(handled, all), Unit::Ratio)],
            breakdown: Vec::new(),
            n: all,
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "counts and seconds stay far below 2^52"
    )]
    fn m10(&mut self) -> Metric {
        let depth: Vec<i64> = self.input.shapes.iter().map(|s| s.depth).collect();
        let age: Vec<i64> = self
            .input
            .shapes
            .iter()
            .map(|s| s.oldest_age_secs)
            .collect();
        let day: Vec<i64> = self.input.shapes.iter().map(|s| s.prior_day).collect();
        let n = depth.len();
        let headroom = |v: Option<i64>| v.map(|v| (v as f64 * BUDGET_HEADROOM).ceil());
        let then = match (
            percentile(&depth, 95),
            percentile(&age, 95),
            percentile(&day, 90),
        ) {
            (Some(_), Some(_), Some(_)) if n >= MIN_SAMPLE => format!(
                "reply_chain max_hops = {}, look-back = {} s, messages = {}",
                headroom(percentile(&depth, 95)).unwrap_or_default(),
                headroom(percentile(&age, 95)).unwrap_or_default(),
                headroom(percentile(&day, 90)).unwrap_or_default(),
            ),
            _ => "keep the starting budgets in docs/design/context-tools.md".into(),
        };
        self.rule(
            "context_budget",
            if n >= MIN_SAMPLE {
                RuleState::Suggestion
            } else {
                RuleState::NotEnoughData
            },
            "P2 budgets start at p95 reach and p90 thread density, with 50 % headroom",
            then,
        );
        let mut summary = pcts(&depth, &[50, 90, 95], Unit::Count);
        for s in &mut summary {
            s.label = format!("hops {}", s.label);
        }
        let mut ages = pcts(&age, &[50, 90, 95], Unit::Seconds);
        for s in &mut ages {
            s.label = format!("age {}", s.label);
        }
        summary.extend(ages);
        Metric {
            id: "M10",
            name: "Reply chain reach",
            summary,
            breakdown: Vec::new(),
            n,
        }
    }

    fn m11(&self) -> Metric {
        let hour: Vec<i64> = self.input.shapes.iter().map(|s| s.prior_hour).collect();
        let day: Vec<i64> = self.input.shapes.iter().map(|s| s.prior_day).collect();
        let mut summary = Vec::new();
        for (label, values) in [("prior hour", &hour), ("prior day", &day)] {
            for p in [50, 90] {
                let mut s = pct(values, p, Unit::Count);
                s.label = format!("{label} {}", s.label);
                summary.push(s);
            }
        }
        Metric {
            id: "M11",
            name: "Thread density before the anchor",
            summary,
            breakdown: Vec::new(),
            n: hour.len(),
        }
    }

    fn latency(
        &self,
        id: &'static str,
        name: &'static str,
        pick: fn(&TicketFact) -> Option<i64>,
    ) -> Metric {
        let mut by_source: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
        for t in self.real_tickets() {
            if let Some(v) = pick(t) {
                by_source.entry(source(t.chat_kind)).or_default().push(v);
            }
        }
        let all: Vec<i64> = by_source.values().flatten().copied().collect();
        Metric {
            id,
            name,
            summary: pcts(&all, &[50, 90], Unit::Seconds),
            breakdown: by_source
                .into_iter()
                .map(|(k, v)| Breakdown {
                    key: k.to_owned(),
                    stats: pcts(&v, &[50, 90], Unit::Seconds),
                    n: v.len(),
                })
                .collect(),
            n: all.len(),
        }
    }

    fn m12(&self) -> Metric {
        self.latency("M12", "Time to read", |t| t.read_secs)
    }

    fn m13(&self) -> Metric {
        self.latency("M13", "Time to reply", |t| t.reply_secs)
    }

    fn up_spans(&self) -> Vec<(i64, i64)> {
        let (since, until) = (self.input.since, self.input.until);
        let mut spans: Vec<(i64, i64)> = self
            .input
            .runs
            .iter()
            .map(|r| (r.started_at.max(since), r.stopped_at.min(until)))
            .filter(|(a, b)| b > a)
            .collect();
        spans.sort_unstable();
        spans
    }

    #[expect(clippy::cast_precision_loss, reason = "seconds stay far below 2^52")]
    fn m14(&mut self) -> Metric {
        let spans = self.up_spans();
        let mut covered = 0;
        let mut cursor = self.input.since;
        for (a, b) in &spans {
            let start = (*a).max(cursor);
            if *b > start {
                covered += b - start;
                cursor = *b;
            }
        }
        let window = (self.input.until - self.input.since).max(1);
        let uptime = (!spans.is_empty()).then(|| covered as f64 / window as f64);
        let gaps = self.gaps();
        let state = match uptime {
            None => RuleState::NotEnoughData,
            Some(u) if u < UPTIME_MIN || !gaps.is_empty() => RuleState::Fires,
            Some(_) => RuleState::Holds,
        };
        self.rule(
            "uptime_low",
            state,
            "uptime under 97 %, or any coverage gap",
            "Check the daemon host before P2: power settings, sleep, crashes in the log.".into(),
        );
        Metric {
            id: "M14",
            name: "Uptime",
            summary: vec![stat("connected", uptime, Unit::Ratio)],
            breakdown: Vec::new(),
            n: spans.len(),
        }
    }

    fn gaps(&self) -> Vec<i64> {
        let spans = self.up_spans();
        spans
            .windows(2)
            .map(|w| w[1].0 - w[0].1)
            .filter(|g| *g > 0)
            .collect()
    }

    #[expect(clippy::cast_precision_loss, reason = "counts stay far below 2^52")]
    fn m15(&self) -> Metric {
        let gaps = self.gaps();
        Metric {
            id: "M15",
            name: "Coverage gaps",
            summary: vec![
                stat("count", Some(gaps.len() as f64), Unit::Count),
                stat(
                    "total",
                    Some(gaps.iter().sum::<i64>() as f64),
                    Unit::Seconds,
                ),
                pct(&gaps, 100, Unit::Seconds),
            ],
            breakdown: Vec::new(),
            n: gaps.len(),
        }
    }

    fn m16(&self) -> Metric {
        let lags = &self.input.update_lags_secs;
        Metric {
            id: "M16",
            name: "Update lag",
            summary: pcts(lags, &[50, 99], Unit::Seconds),
            breakdown: Vec::new(),
            n: lags.len(),
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "counts and seconds stay far below 2^52"
    )]
    fn m17(&mut self) -> Metric {
        let mut by_method: BTreeMap<(&str, ApiTrouble), (usize, i64)> = BTreeMap::new();
        for e in &self.input.api {
            let entry = by_method.entry((e.method.as_str(), e.kind)).or_default();
            entry.0 += 1;
            if e.kind == ApiTrouble::FloodWait {
                entry.1 += e.value;
            }
        }
        let worst_wait = self
            .input
            .api
            .iter()
            .filter(|e| e.kind == ApiTrouble::FloodWait)
            .map(|e| e.value)
            .max();
        self.rule(
            "flood_wait",
            if worst_wait.is_some_and(|w| w > FLOOD_WAIT_MAX_SECS) {
                RuleState::Fires
            } else {
                RuleState::Holds
            },
            "any FLOOD_WAIT over 60 s",
            "Find the caller; context tools must read the store first.".into(),
        );
        let floods = self
            .input
            .api
            .iter()
            .filter(|e| e.kind == ApiTrouble::FloodWait)
            .count();
        Metric {
            id: "M17",
            name: "API pressure",
            summary: vec![
                stat("flood waits", Some(floods as f64), Unit::Count),
                stat(
                    "rpc errors",
                    Some((self.input.api.len() - floods) as f64),
                    Unit::Count,
                ),
            ],
            breakdown: by_method
                .into_iter()
                .map(|((method, kind), (n, secs))| Breakdown {
                    key: format!("{method} {}", kind_name(kind)),
                    stats: match kind {
                        ApiTrouble::FloodWait => vec![
                            stat("count", Some(n as f64), Unit::Count),
                            stat("waited", Some(secs as f64), Unit::Seconds),
                        ],
                        ApiTrouble::RpcError => vec![stat("count", Some(n as f64), Unit::Count)],
                    },
                    n,
                })
                .collect(),
            n: self.input.api.len(),
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "bytes stay far below 2^52")]
    fn m18(&self) -> Metric {
        let mut sizes = self.input.store_sizes.clone();
        sizes.sort_unstable();
        let latest = sizes.last().map(|(_, b)| *b as f64 / BYTES_PER_MB);
        let growth = match (sizes.first(), sizes.last()) {
            (Some((t0, b0)), Some((t1, b1))) if t1 > t0 => {
                let days = (t1 - t0) as f64 / SECS_PER_DAY as f64;
                Some((b1 - b0) as f64 / BYTES_PER_MB / days)
            }
            _ => None,
        };
        Metric {
            id: "M18",
            name: "Store growth",
            summary: vec![
                stat("size", latest, Unit::Megabytes),
                stat("growth", growth, Unit::MegabytesPerDay),
            ],
            breakdown: Vec::new(),
            n: sizes.len(),
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "counts stay far below 2^52")]
    fn m19(&self) -> Metric {
        let top = top_candidates(&self.input.candidates);
        Metric {
            id: "M19",
            name: "Out-of-scope chats that would trigger",
            summary: vec![stat("chats", Some(top.len() as f64), Unit::Count)],
            breakdown: top
                .iter()
                .map(|c| Breakdown {
                    key: c.title.clone().unwrap_or_else(|| c.chat.0.to_string()),
                    stats: vec![
                        stat(
                            "would trigger per day",
                            Some(c.would_trigger as f64 / self.days),
                            Unit::PerDay,
                        ),
                        stat(
                            "messages per day",
                            Some(c.messages as f64 / self.days),
                            Unit::PerDay,
                        ),
                    ],
                    n: c.would_trigger,
                })
                .collect(),
            n: self.input.candidates.len(),
        }
    }
}

/// Skip reason for a burst the owner cancelled by writing on the thread.
pub const SELF_HANDLED: &str = "self_handled";

fn kind_name(kind: ApiTrouble) -> &'static str {
    match kind {
        ApiTrouble::FloodWait => "flood wait",
        ApiTrouble::RpcError => "error",
    }
}

fn top_candidates(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by(|a, b| {
        b.would_trigger
            .cmp(&a.would_trigger)
            .then(a.chat.cmp(&b.chat))
    });
    sorted.truncate(CANDIDATES_SHOWN);
    sorted
}

fn daily(input: &ReportInput) -> Vec<DailyTickets> {
    let first = unix_day(input.since);
    let last = unix_day(input.until.max(input.since));
    let mut days: BTreeMap<i64, DailyTickets> = (first..=last)
        .map(|day| {
            (
                day,
                DailyTickets {
                    day,
                    draft: 0,
                    digest: 0,
                    skipped: 0,
                },
            )
        })
        .collect();
    for t in &input.tickets {
        let Some(d) = days.get_mut(&unix_day(t.opened_at)) else {
            continue;
        };
        match t.route {
            Some(Route::Draft) => d.draft += 1,
            Some(Route::Digest) => d.digest += 1,
            None => d.skipped += 1,
        }
    }
    days.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use captain_core::MessageId;

    const DAY: i64 = SECS_PER_DAY;

    fn ticket(
        id: i64,
        trigger: TriggerKind,
        opened_at: i64,
        implicit: ImplicitLabel,
    ) -> TicketFact {
        TicketFact {
            id,
            key: ThreadKey {
                chat: ChatId(-1),
                topic: Some(MessageId(i32::try_from(id).unwrap())),
            },
            chat_label: "ops".into(),
            chat_kind: Some(EntryKind::Group),
            profile: "group".into(),
            trigger,
            route: Some(Route::Draft),
            state: TicketState::Resolved,
            reason: None,
            opened_at,
            closed_at: opened_at + 10,
            anchor_date: opened_at,
            messages: 2,
            implicit: Some(implicit),
            explicit: None,
            read_secs: Some(60),
            reply_secs: implicit.is_answer().then_some(300),
        }
    }

    fn metric<'a>(r: &'a Report, id: &str) -> &'a Metric {
        r.sections
            .iter()
            .flat_map(|s| &s.metrics)
            .find(|m| m.id == id)
            .unwrap()
    }

    fn rule<'a>(r: &'a Report, id: &str) -> Vec<&'a Rule> {
        r.rules.iter().filter(|x| x.id == id).collect()
    }

    #[test]
    fn empty_input_reports_no_data_without_panicking() {
        let r = build(
            &ReportInput {
                since: 0,
                until: 14 * DAY,
                ..ReportInput::default()
            },
            &Policy::default(),
        );
        assert_eq!(r.sections.len(), 7);
        assert_eq!(r.daily.len(), 15);
        assert!(metric(&r, "M4").summary[0].value.is_none());
        assert_eq!(rule(&r, "volume_low")[0].state, RuleState::NotEnoughData);
    }

    #[test]
    fn precision_rule_fires_for_a_noisy_trigger() {
        let mut tickets = Vec::new();
        for i in 0..12 {
            let label = if i < 2 {
                ImplicitLabel::Answered
            } else {
                ImplicitLabel::Unseen
            };
            tickets.push(ticket(i, TriggerKind::Keyword, i * 100, label));
            tickets.push(ticket(
                100 + i,
                TriggerKind::Mention,
                i * 100,
                ImplicitLabel::Answered,
            ));
        }
        let r = build(
            &ReportInput {
                since: 0,
                until: DAY,
                tickets,
                ..ReportInput::default()
            },
            &Policy::default(),
        );
        let rules = rule(&r, "trigger_precision");
        let fired: Vec<_> = rules
            .iter()
            .filter(|r| r.state == RuleState::Fires)
            .collect();
        assert_eq!(fired.len(), 1);
        assert!(fired[0].then.contains("keyword"));
        assert_eq!(metric(&r, "M2").summary[1].value, Some(24.0));
        assert_eq!(rule(&r, "volume_high")[0].state, RuleState::Holds);
    }

    #[test]
    fn debounce_suggestion_uses_p90_plus_margin_within_bounds() {
        let gaps = (1..=20).map(|g| ("dm".to_owned(), g * 2)).collect();
        let r = build(
            &ReportInput {
                since: 0,
                until: DAY,
                burst_gaps: gaps,
                ..ReportInput::default()
            },
            &Policy::default(),
        );
        let debounce = rule(&r, "debounce");
        assert_eq!(debounce[0].state, RuleState::Suggestion);
        assert!(
            debounce[0].then.contains("debounce_secs = 41"),
            "{}",
            debounce[0].then
        );
    }

    #[test]
    fn uptime_counts_overlaps_once_and_gaps_fire() {
        let runs = vec![
            RunSpan {
                started_at: 0,
                stopped_at: 50,
            },
            RunSpan {
                started_at: 40,
                stopped_at: 60,
            },
            RunSpan {
                started_at: 80,
                stopped_at: 100,
            },
        ];
        let r = build(
            &ReportInput {
                since: 0,
                until: 100,
                runs,
                ..ReportInput::default()
            },
            &Policy::default(),
        );
        assert_eq!(metric(&r, "M14").summary[0].value, Some(0.8));
        assert_eq!(metric(&r, "M15").summary[0].value, Some(1.0));
        assert_eq!(rule(&r, "uptime_low")[0].state, RuleState::Fires);
    }

    #[test]
    fn reopen_and_self_handled_are_measured() {
        let mut a = ticket(1, TriggerKind::Mention, 0, ImplicitLabel::Answered);
        let mut b = ticket(2, TriggerKind::Mention, 200, ImplicitLabel::Answered);
        b.key = a.key;
        a.closed_at = 100;
        let mut c = ticket(3, TriggerKind::Mention, 300, ImplicitLabel::Answered);
        c.reason = Some(SELF_HANDLED.into());
        c.route = None;
        let r = build(
            &ReportInput {
                since: 0,
                until: DAY,
                tickets: vec![a, b, c],
                ..ReportInput::default()
            },
            &Policy::default(),
        );
        assert_eq!(metric(&r, "M8").summary[0].value, Some(0.5));
        let m9 = metric(&r, "M9").summary[0].value.unwrap();
        assert!((m9 - 1.0 / 3.0).abs() < 1e-9);
    }
}
