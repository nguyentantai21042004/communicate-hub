# Triage and tickets

## Triage

Triage turns a closed burst into a ticket with a route, using fixed rules in Rust. No model runs
here (D5).

| Rule | Result |
|---|---|
| Sender is a bot, or every message is Tài's | drop |
| Tài sent a message on the same key after the anchor | `skip: self_handled` |
| Anchor is older than `stale_after_secs` when the burst closes (catch-up after sleep) | route `digest` |
| Trigger kind routes to digest (keyword) | route `digest` |
| Chat is muted or the pipeline is paused | `skip: paused` |
| Otherwise | route `draft` |

Whether the message is Tài's business, and which domain it belongs to, is decided by the runner
in P2 (D7). In P1 every `draft`-routed ticket stops at `queued` and waits for its implicit
label; nothing runs.

## Ticket

A ticket is a pointer, plus a seed small enough to save the runner two obvious tool calls (D6).

| Field | Meaning |
|---|---|
| `ticket_id` | Monotonic, local |
| `chat_id`, `topic_id` | Where the reply will go |
| `anchor_msg_ids` | Every message in the burst |
| `trigger`, `reason` | Which rule fired and why, for example `mention` / `@tantai in msg 812` |
| `route` | `draft` or `digest` |
| `seed` | The last triggering message and the message it replies to, at most two |
| `note` | Tài's `cap:` text for manual tickets |
| `stale` | Anchor was older than the threshold |

The seed holds no other history. Everything else is pulled.

While a ticket on a key is `queued` or `running`, a new burst on the same key is merged into it
instead of opening a second ticket. The chat has one draft slot per topic, so two tickets on the
same key would fight over it.

## Lifecycle

```
open burst ──close──> triaged ─┬─> dropped / skipped (reason)
                               ├─> digest
                               └─> queued ──> running ─┬─> drafted ─┬─> sent_as_is
                                  (P1 stops here)      │            ├─> sent_edited
                                                       │            ├─> discarded
                                                       │            └─> expired
                                                       ├─> declined (not_mine, no_reply_needed)
                                                       └─> failed (after retries)
```

Every transition is a row in `ticket_events`, so the report can reconstruct timing.

## Resolution in P1

With no drafts in P1, a resolver job labels each queued ticket from what Tài did
(`label_implicit`, see [metrics-p1.md](metrics-p1.md#labels)):

| Label | Condition, within `resolve_window_secs` of the anchor |
|---|---|
| `answered` | Tài sent a message on the same key |
| `answered_elsewhere` | No reply on the key, but Tài messaged the asker's DM |
| `read_only` | Tài's read marker passed the anchor, no reply |
| `unseen` | Neither read nor answered |

The resolver runs hourly and resolves tickets older than the window, 24 hours by default.
