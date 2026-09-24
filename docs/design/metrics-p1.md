# Metrics plan for P0 and P1

P1 runs the daemon, scope, triggers and tickets for at least 14 days with no model involved.
Its job is to measure real traffic, so that P2 starts from numbers instead of guesses. P0
comes first and answers a few one-off protocol questions against the live account.

## Questions P1 must answer

Each question maps to a decision that is blocked until it has an answer.

| # | Question | Decision it unblocks |
|---|---|---|
| Q1 | How much arrives, per chat and trigger kind? | Whether P2 is worth building, and how tight the scope must be |
| Q2 | Which triggers point at messages Tài actually answers? | Which trigger kinds route to drafts and which to digest |
| Q3 | Which messages Tài answered did no trigger catch? | Missing trigger rules |
| Q4 | What do bursts look like? | `debounce_secs`, `burst_max_*` per profile |
| Q5 | How much history does a triggered message depend on? | Starting budgets for context tools |
| Q6 | How fast does Tài read and reply today? | The baseline P2 must beat |
| Q7 | Is the daemon reliable on the second Mac? | Whether P2 can depend on it |

## Principles

- Record events, compute metrics at report time. A new metric then works on old data, and
  nothing is lost to a counter that was defined wrong.
- Metrics never contain message text. Ids, counts, durations and tags only.
- Labels are mostly implicit, taken from what Tài does anyway. An explicit label is a short
  optional daily review, so the measurement does not depend on discipline.
- Most numbers come from tables that exist for other reasons (`messages`, `tickets`,
  `ticket_events`, `daemon_runs`). `metric_events` holds only what those cannot.
- 14 days minimum, covering two working weeks, so one unusual day does not decide a threshold.

## Data sources

| Source | Gives | Already needed for |
|---|---|---|
| `messages.date`, `messages.received_at` | Update lag, inter-arrival gaps, Tài's reply times | store |
| `tickets`, `ticket_events`, `ticket_messages` | Volume, routes, burst size and span, labels | pipeline |
| `daemon_runs` | Uptime, gaps, catch-up | daemon |
| `gate_drops` | Out-of-scope volume and candidates | scope curation |
| `metric_events` | The rest, listed below | metrics only |

`metric_events` kinds:

| Kind | Emitted when | `value_a` | `value_b` | `tag` |
|---|---|---|---|---|
| `read_advanced` | A read-inbox update for a scoped chat | new max read id | unread count left | topic if known |
| `catch_up` | After `getDifference` on start or wake | messages delivered | seconds offline | `ok`, `too_long` |
| `flood_wait` | `FLOOD_WAIT_X` from any call | seconds | | method |
| `rpc_error` | Any other RPC error | error code | | method and error name |
| `reply_target_fetch` | `reply_to_me` needed an API fetch | target age secs | | `hit`, `miss` |
| `context_shape` | A ticket closes | reply chain depth | age of the oldest message in the chain, secs | `quote`, `media`, `both`, `none` |
| `context_density` | A ticket closes | messages on the key in the prior hour | messages on the key in the prior 24 h | |
| `store_size` | Daily | database bytes | message rows | |
| `outgoing_dm` | Tài sends a DM, in or out of scope | | | |

`outgoing_dm` stores only the recipient and the time. It is how the resolver tells
`answered_elsewhere` apart from `read_only` when the DM is outside scope.

`context_shape` and `context_density` are computed from the store at close time. They answer Q5
without running a model: how far back a triggered message reaches, and how busy its thread is.

## Labels

Implicit, set by the hourly resolver 24 hours after the anchor (see
[tickets.md](tickets.md#resolution-in-p1)):

| Label | Meaning |
|---|---|
| `answered` | Tài replied on the same chat and topic |
| `answered_elsewhere` | Tài wrote to the asker's DM instead |
| `read_only` | Read, not answered |
| `unseen` | Not read, not answered |

`answered` is the proxy for "a draft would have been useful". It overcounts ("ok" replies) and
undercounts (answers given in a call), which is why an explicit sample exists.

Explicit, optional: the Tickets screen of the [monitoring UI](monitor.md) has a Not labelled by
me filter and Want, Noise and Skip buttons on each ticket. `captain ticket label <id> want`
does the same from a terminal. The target is about 50 labelled tickets over the period, or all
of them if volume is lower.

## Metrics

| Id | Metric | Definition | Answers |
|---|---|---|---|
| M1 | Scoped inbound | Incoming messages per day, per chat and chat kind | Q1 |
| M2 | Ticket volume | Tickets per day, per trigger kind and route | Q1 |
| M3 | Coalescing ratio | Messages per ticket | Q1, Q4 |
| M4 | Implicit precision | (`answered` + `answered_elsewhere`) / resolved tickets, per trigger kind | Q2 |
| M5 | Explicit precision | `w` / labelled tickets, per trigger kind | Q2 |
| M6 | Recall | Messages from others that Tài replied to (his outgoing `reply_to` points at them, or he wrote on the key within 30 min) and that belong to some ticket, over all such messages | Q3 |
| M7 | Burst gaps | p50, p90, p95 of the gap between consecutive incoming messages on a key within 5 min of each other | Q4 |
| M8 | Reopen rate | Tickets opened on a key within 10 min of the previous ticket on that key closing, over all tickets | Q4 |
| M9 | Self-handled rate | Bursts cancelled because Tài wrote during them | Q4 |
| M10 | Context reach | p50, p90, p95 of reply chain depth and of oldest-chain-message age, from `context_shape` | Q5 |
| M11 | Thread density | p50, p90 of `context_density` | Q5 |
| M12 | Time to read | Anchor `date` to the first `read_advanced` covering it, p50 and p90 per source | Q6 |
| M13 | Time to reply | Anchor `date` to Tài's first message on the key, p50 and p90 per source | Q6 |
| M14 | Uptime | Connected time over wall time since the first recorded run, from `daemon_runs` | Q7 |
| M15 | Coverage gaps | Count and total length of gaps that catch-up did not fill | Q7 |
| M16 | Update lag | p50, p99 of `received_at - date` for live messages | Q7 |
| M17 | API pressure | `flood_wait` count and seconds, `rpc_error` count, per method | Q7 |
| M18 | Store growth | MB per day from `store_size` | Q7 |
| M19 | Scope candidates | Top 10 out-of-scope chats by `would_trigger` count in `gate_drops` | Q1 |

## Decision rules

Starting rules. The report prints which ones fire; Tài makes the call.

| When | Then |
|---|---|
| Draft-routed tickets exceed 40 a day | Tighten scope before P2. Review fatigue and cost grow with volume. |
| Draft-routed tickets are under 3 a day | P2 saves little. Widen scope with M19 first. |
| A trigger kind has M4 under 30 % and M5 under 50 % | Route that kind to digest |
| M6 under 80 % | Look at the missed messages and add or fix a trigger before P2 |
| M8 over 10 % | Raise `debounce_secs` for that profile |
| Always | `debounce_secs` = p90 of M7 plus 5 s, within 15 s to 120 s |
| Always | Context budgets start at p95 of M10 for hops and horizon, and p90 of M11 for messages, each with 50 % headroom |
| M14 under 97 %, or any unexplained M15 gap | Fix the daemon host before P2 |
| M17 shows any `FLOOD_WAIT` over 60 s | Find the caller; context tools must stay store-first |

M12 and M13 are not rules. They are the baseline: in P2 the question becomes whether Tài
answers faster with drafts than he does now.

## Report

`captain report --since 14d` prints each metric with its period, grouped by question, then the
decision rules that fire. `--json` gives the same for scripts. No message text appears unless
`--show-text` is passed. It reads the store through the daemon, so it is safe while the daemon
runs.

## P0 live checks

One-off, run by Tài against the live account before P1. Each one changes a design choice if
the answer is not the expected one.

| Check | How | Expected | If not |
|---|---|---|---|
| Reply to Tài sets `mentioned` | A second account replies to Tài's group message without `@` | Unknown; this is the question | If set, `reply_to_me` can rely on the flag and skip the store lookup |
| Daemon does not show Tài online | Phone offline, daemon running 1 h, second account watches last seen | Last seen unchanged | Find what sends a status and stop it before P1 |
| Reading history keeps unread badges | Daemon pulls history of an unread chat | Badge unchanged on the phone | Stop; this breaks API terms 1.4 |
| Forum topic fields | Messages in General, a topic, and a reply inside a topic | As in [scope.md](scope.md#topics) | Fix the topic derivation |
| DM delete has no chat id | Delete a DM message | `chat` is `None`, store lookup finds it | Adjust the delete path |
| Read events arrive from other devices | Read a scoped chat on the phone | `updateReadHistoryInbox` or `updateReadChannelInbox` reaches the daemon; note topic granularity | M12 is dropped or computed per chat only |
| Draft appears and sends correctly | `saveDraft` with `reply_to` in a DM and in a topic | Draft shows on phone and desktop, sends as a reply in the right topic | Rework [review.md](review.md) |
| Draft while typing | `saveDraft` while the phone editor is open with text | Document what happens | Tighten the overwrite rule |
| Media draft | `saveDraft` with `media` | Document whether apps show it | Keep drafts text only |
| Catch-up after sleep | Close the lid 1 h, messages arrive, open | All messages delivered via `getDifference` | Coverage gap handling gets tested for real |

## Build order inside P1

1. `MessageView` and `EventKind` fields from [README.md](README.md#changes-to-existing-crates).
2. Daemon with the session lock and socket; `send`, `download`, `listen` through it.
3. Store with `messages`, `chats`, `daemon_runs`, retention.
4. Scope gate and `gate_drops`, `captain chats`, `pause` and `resume`.
5. Triggers, coalescing, triage, tickets.
6. `metric_events` and the resolver job.
7. `captain report` and the monitoring UI.

Metrics start counting on the day step 5 ships. The 14 days run from then.

## Exit from P1

- Every question Q1 to Q7 has a number in the report.
- The decision rules have been reviewed and the resulting `policy.toml` and `scope.toml` are
  committed to Tài's config.
- The API terms 1.5 decision in [risks.md](risks.md) is made. P2 does not start without it.
