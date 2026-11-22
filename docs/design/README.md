# Inbox design

Status: designed, not built. Layer 1 (`captain-protocol`) exists and is described in
`docs/architecture.md`. Everything in this directory sits on top of it.

## Goal

A message that needs Tài's answer becomes a draft reply in the right chat, written by Claude
with enough context. Tài reads the draft in the official Telegram app, edits it, and sends it
himself.

## Non-goals for v1

- Sending anything automatically. The system writes drafts; a person presses Send.
- Secret chats. They are end-to-end encrypted per device and never reach the cloud API.
- Training, fine-tuning or evaluating models on message data.
- More than one account per daemon.
- A custom review UI. The Telegram app's own draft is the review surface.

## Principles

1. A mention is a signal, not a payload. A ticket is a pointer to messages. Claude pulls the
   context it needs through tools, inside a budget the daemon enforces.
2. Default deny. Nothing from a chat outside the scope list is stored or processed.
3. One process owns the Telegram session. Everything else talks to that process.
4. The human sends. Nothing in the pipeline can send a message to another person.
5. Measure before tuning. Every policy number lives in config and changes through a report
   that Tài reviews.

## System

```
 Telegram updates ─┐
                   │        captaind: the only process holding the MTProto session
                   ▼  ┌───────────────────────────────────────────────────────────────┐
                ingest ─> scope gate ─> triggers ─> triage ─> ticket ─> queue          │
                   │      default deny   per source   rules     pointer     │          │
                   ▼                                    │       + seed      ▼          │
              SQLite store ◄──────────────────── digest ┘            runner (P2)       │
              scoped chats only, TTL                                  claude -p        │
                   ▲                                                      │            │
                   │  local first, API for gaps                           ▼            │
                   └──── IPC, Unix socket 0600 ◄──── captain mcp: read tools + draft   │
                                    │                                                  │
                                    ▼                                                  │
                        messages.saveDraft in the source chat ──> Tài edits, sends     │
                                    │                                                  │
                        outcome tracker ──> metrics ──> captain report ──> optimizer   │
                      └───────────────────────────────────────────────────────────────┘
 Manual: forward to Saved Messages with a `cap:` note, or `captain ticket new`
```

## Components

| Component | Lives in | Doc | Phase |
|---|---|---|---|
| Daemon and IPC | `captain daemon`, `captain-ipc` | [daemon.md](daemon.md) | P1 |
| Store | `captain-store` | [store.md](store.md) | P1 |
| Scope gate | `captain-inbox::scope` | [scope.md](scope.md) | P1 |
| Triggers and coalescing | `captain-inbox::trigger` | [triggers.md](triggers.md) | P1 |
| Triage and tickets | `captain-inbox::ticket` | [tickets.md](tickets.md) | P1 |
| Metrics | `captain-store` tables, `captain report` | [metrics-p1.md](metrics-p1.md) | P1 |
| Monitoring UI | `captain daemon`, `captain ui` | [monitor.md](monitor.md) | P1 |
| Context tools | `captain mcp` | [context-tools.md](context-tools.md) | P2 |
| Runner | `captain daemon` | [runner.md](runner.md) | P2 |
| Review and outcomes | `captain-protocol` drafts, `captain-inbox::outcome` | [review.md](review.md) | P2 |
| Optimizer | `captain report --propose` | [optimizer.md](optimizer.md) | P3 |

[risks.md](risks.md) covers the Telegram API terms, data handling and prompt injection.

## Crate graph after P2

```
captain-cli ──┬──> captain-inbox ─────> captain-core
              ├──> captain-store ──┬──> captain-core
              │                    └──> captain-inbox
              ├──> captain-ipc ───────> captain-core
              ├──> captain-protocol ──┬─> captain-core
              │                       └─> captain-vault
              ├──> captain-format ────> captain-core
              └──> captain-vault
```

- `captain-inbox` is pure logic: scope matching, trigger rules, coalescing, ticket state,
  outcome matching. It does no I/O, reads no clock (time is passed in), and depends on no
  Telegram or SQLite library. Everything it needs to know, such as "is the replied-to message
  one of mine", arrives as an input the daemon looked up.
- `captain-store` owns SQLite and knows nothing about grammers. It uses `captain-inbox` types
  (ticket states, labels, thread keys) and builds the report input that `captain-inbox` turns
  into numbers.
- `captain-ipc` defines the socket protocol as core types plus request and response enums.
- The daemon, the MCP server and the CLI are subcommands of the one `captain` binary in
  `apps/captain-cli`, the only place allowed to depend on everything.

## Changes to existing crates

`captain_core::MessageView` has to carry what triggers and context tools read:

| Field | From TL | Used by |
|---|---|---|
| `mentioned` | `message.mentioned` | mention trigger |
| `topic` | `messageReplyHeader.forum_topic`, `reply_to_top_id`, `reply_to_msg_id` | scope, coalescing key |
| `quote_text` | `messageReplyHeader.quote_text` | seed, context |
| `forward` | `messageFwdHeader.saved_from_peer`, `saved_from_msg_id`, `from_id`, `date` | manual tickets |
| `edited_at_unix` | `message.edit_date` | re-triage on edit |
| `sender.is_bot` on `ChatSummary` | `user.bot` | triage skip rule |

`EventKind` gains a read-inbox variant built from `updateReadHistoryInbox`,
`updateReadChannelInbox` and `updateReadChannelDiscussionInbox`, and a draft variant from
`updateDraftMessage`. `captain-protocol` gains history, replies, search, dialog-draft read and
`save_draft`.

## Decisions

| # | Decision | Doc |
|---|---|---|
| D1 | One daemon owns the session; CLI and MCP are clients over a Unix socket; a lock file stops a second owner | daemon |
| D2 | SQLite store of scoped chats only, 30-day text retention | store |
| D3 | Scope by peer id, default deny, runtime pause switch | scope |
| D4 | Trigger rules per source kind; coalesce bursts per chat and topic | triggers |
| D5 | Triage is deterministic Rust; no model before the runner | tickets |
| D6 | A ticket is a pointer plus a seed of at most two messages | tickets |
| D7 | Runner is headless Claude Code, one session per ticket, queue of one, read-only tools plus `save_draft` | runner |
| D8 | Review happens in the native cloud draft; no send path | review |
| D9 | Draft outcomes are labelled automatically from what Tài sends | review |
| D10 | Manual tickets come from Saved Messages forwards with a `cap:` note, or the CLI | triggers |
| D11 | New crates keep the one-way dependency direction above | this file |
| D12 | The optimizer runs offline and proposes config diffs for review | optimizer |

## Where this differs from the first sketch

1. The ticket carries a seed of the trigger message and its parent. Every tool call costs a
   full model turn, and every run needs those two anyway.
2. Triage does not decide the domain or whether Tài is in charge. That needs the roster and
   reasoning, which is the runner's first step. A cheap pre-classifier comes back only if P2
   numbers show the runner wasting money on tickets it then declines.
3. There is no automatic reply path. Tài sends from the official app. A one-tap approve can be
   discussed once a ticket type shows a high "sent as is" rate.
4. The optimizer does not change policy while running. Volume is a few dozen tickets a day, and
   labels are noisy; online tuning would chase one odd week.

## Phases

| Phase | Scope | Exit criterion |
|---|---|---|
| P0 | Live login, `listen`, `send`; revoke the old Telethon session; one-off live checks | All checks in [metrics-p1.md](metrics-p1.md#p0-live-checks) answered |
| P1 | Daemon, store, scope, triggers, tickets, metrics, `captain report`. No Claude | 14 days of data and the P1 decisions made |
| P2 | Context tools, runner, drafts, outcome tracker | Share of drafts sent, edited or not, is known per trigger kind |
| P3 | Digest, keyword and soft triggers, optimizer proposals | Trigger precision improves across two review cycles |

P1 leaves Claude out on purpose. Tuning a pull policy before knowing the real traffic is
guessing.

## Open questions

- Whether Tài accepts the API terms 1.5 risk. See [risks.md](risks.md). It gates P2.
- Initial scope list, and whether work ops chats belong in it.
- Whether the second Mac may hold clones of work repos for the runner.
- What "Jev" in the sketch refers to. See [optimizer.md](optimizer.md).
