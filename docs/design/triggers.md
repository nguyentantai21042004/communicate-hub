# Triggers and coalescing

## Responsibility

Turn in-scope messages into candidate tickets. A trigger answers "should someone look at
this", not "what should the answer be".

## Trigger kinds

| Kind | Fires when | Evidence | Default route |
|---|---|---|---|
| `dm_all` | Any incoming message in a scoped DM | chat kind | draft |
| `mention` | `message.mentioned` is set | TL flag, covers `@username` and mention-by-name | draft |
| `reply_to_me` | `reply_to_msg_id` points at a stored message with `outgoing = true` | store lookup | draft |
| `keyword` | Normalized text matches a keyword of that chat | text | digest |
| `manual` | A `cap:` note in Saved Messages, or `captain ticket new` | see below | draft, bypasses scope |
| `schedule` (P3) | Digest timer | clock | digest |

Notes:

- The mentions doc lists `@username` and mention-by-name. It does not say whether a reply to
  Tài's message sets `mentioned`, so `reply_to_me` checks the store itself. If the reply target
  is not stored (older than retention, or from before the chat entered scope), the daemon may
  fetch that one message with `messages.getMessages` or `channels.getMessages`. P0 checks
  whether the flag makes this unnecessary.
- Keyword matching lowercases and strips Vietnamese diacritics on both sides (NFD, drop
  combining marks), so `hoan tien` and `hoàn tiền` match.
- Messages from bots, and Tài's own messages, never trigger.

## Manual tickets

TELEGRAM: `messageFwdHeader.saved_from_peer` and `saved_from_msg_id` are set only for messages
forwarded to Saved Messages (constructor docs for `messageFwdHeader`). A forward into any other
chat loses the pointer to the original message. So:

- Tài forwards one or more messages to Saved Messages and adds a note that starts with `cap:`.
  The apps send a forward comment as a separate message just before the forwards.
- The daemon reads Saved Messages only for this pattern: a `cap:` note plus the forwarded
  messages sent within a short window of it. The forwards give `(saved_from_peer,
  saved_from_msg_id)` pointers. The note text becomes the instruction for the runner.
- Anything else in Saved Messages is ignored and never stored.
- `captain ticket new --chat <id> --message <id> [--note ...]` does the same from the terminal.

A manual ticket skips the scope gate for the pointed chat, because Tài chose it. Context tools
may then read that chat through the API for this ticket only, and do not store what they read.

## Coalescing

People write in bursts: "anh ơi", "cái này", a screenshot, then the question. One burst should
be one ticket.

- Key: `(chat_id, topic_id)`.
- The first triggering message opens a burst. Any later incoming message on the same key joins
  it, whether it triggers or not, as long as it arrives within `debounce_secs` of the previous
  one.
- The burst closes when `debounce_secs` pass with no new message, or at `burst_max_secs` or
  `burst_max_messages`, whichever comes first.
- An outgoing message from Tài on that key during the burst cancels it with reason
  `self_handled`. He is already answering.
- Album members (same `grouped_id`) count as one message.

Starting values, per profile in `policy.toml`, to be replaced by P1 measurements:

| Profile | `debounce_secs` | `burst_max_secs` | `burst_max_messages` |
|---|---|---|---|
| `dm` | 45 | 300 | 30 |
| `group` | 10 | 120 | 20 |

The closed burst goes to triage as one candidate with every message id in it.

## Edits and deletes

- An edit to a message in an open burst updates it in place.
- An edit to the anchor of a closed ticket re-runs triage. If a draft exists, the ticket is
  flagged `anchor_edited` so the review step can mark the draft stale.
- Deleting the anchor cancels an open ticket. An existing draft is left for Tài.
