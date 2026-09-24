# Store

## Responsibility

A local SQLite database that remembers scoped messages, tickets, drafts and metrics. Telegram
stays the source of truth for messages; the store is a cache plus a ledger of what the system
did.

It exists because:

- A reply-to-me trigger needs to know whether the replied-to message was Tài's.
- Delete updates in private chats and basic groups carry no chat id (`updateDeleteMessages`).
  Message ids there come from one counter per account, so the store maps id to chat.
- Context tools answer most pulls locally, which avoids `FLOOD_WAIT` and is faster.
- Tickets, drafts and metrics need somewhere durable across restarts.

## Location and format

- `<state dir>/inbox.sqlite`, mode `0600`, WAL journal.
- `rusqlite` with the `bundled` feature, so the macOS system SQLite version does not matter.
  No ORM and no async driver: writes go through the store writer thread in the daemon.
- Excluded from Time Machine with `tmutil addexclusion`, so message text does not outlive its
  retention in backups.
- Not encrypted beyond FileVault in v1. SQLCipher would add a key to manage in the Keychain for
  data that already sits on an encrypted disk under the same user.

## Tables

Columns listed are the ones decisions depend on; the migration adds bookkeeping as needed.

| Table | Key | Main columns | Retention |
|---|---|---|---|
| `chats` | `chat_id` | kind, title, username, first_seen, last_seen | while in scope |
| `messages` | `chat_id, msg_id` | topic_id, sender_id, sender_is_bot, outgoing, date, received_at, edit_date, reply_to_msg_id, quote_text, mentioned, album_id, text, media_kind, deleted_at | text 30 days, then row deleted |
| `coverage` | `chat_id, from_ts` | to_ts, source (live, catch-up) | 30 days |
| `tickets` | `ticket_id` | chat_id, topic_id, trigger, route, state, opened_at, closed_at, resolved_at, label_implicit, label_explicit | 180 days, ids and labels only |
| `ticket_messages` | `ticket_id, msg_id` | role (anchor, seed, burst) | with ticket |
| `ticket_events` | `ticket_id, seq` | at, from_state, to_state, reason | with ticket |
| `drafts` (P2) | `draft_id` | ticket_id, chat_id, topic_id, reply_to, text, text_hash, saved_at, outcome, outcome_at, edit_ratio | text 30 days |
| `runs` (P2) | `run_id` | ticket_id, claude_session_id, started_at, ended_at, tool_calls, messages_pulled, used_ids, tokens_in, tokens_out, cost_usd, exit | 180 days |
| `metric_events` | `seq` | at, kind, chat_id, topic_id, ticket_id, value_a, value_b, tag | 180 days, never text |
| `gate_drops` | `hour, chat_id` | messages, would_trigger | 30 days |
| `daemon_runs` | `started_at` | stopped_at, stop_reason | 180 days |

`received_at` next to `date` gives update lag without a separate event. `metric_events` is
described in [metrics-p1.md](metrics-p1.md).

## Rules

- Only the scope gate decides what enters `messages`. Tài's own outgoing messages in scoped
  chats are stored, because triggers and outcomes need them.
- `gate_drops` is the one place an out-of-scope chat id is recorded, as an hourly count with no
  text. It powers the "chats you might want to add" part of the report. Config can turn it off.
- An edit overwrites text and sets `edit_date`. A delete sets `deleted_at` and clears text.
- Retention runs nightly. Deleting text deletes it; there is no soft copy.

## Coverage

A context pull needs to know whether the store has every message in a range or only some.

- While connected, the daemon extends a `live` coverage interval for every scoped chat.
- A disconnect ends the interval. A successful catch-up adds a `catch-up` interval over the
  gap. `differenceTooLong` or `updateChannelTooLong` leaves the gap open.
- A chat added to scope has no coverage before the moment it was added.
- Context tools read locally when the requested range is inside coverage, and call the API for
  the rest. API results are written back only for scoped chats.

Channel and supergroup ids are contiguous per chat, but deleted messages leave holes, so gaps
cannot be inferred from ids alone. Time intervals are the reliable signal.
