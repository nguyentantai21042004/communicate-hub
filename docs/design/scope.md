# Scope gate

## Responsibility

Decide, for every inbound event, whether the system may look at it. It runs first, before the
store and before triggers. An event outside scope is dropped with at most an hourly count in
`gate_drops`.

## Rules

- Default deny. A chat is in scope only if `scope.toml` lists it.
- Entries are keyed by marked peer id, the same `ChatId` the CLI prints. Usernames change and
  can be taken over; ids do not.
- A group entry covers the whole group, or only the listed forum topics.
- Each entry names the triggers it enables and a policy profile (`dm`, `group`, `ops`, ...).
  Profiles hold debounce, budget and routing numbers, defined in `policy.toml`.
- Saved Messages is not in scope. Only the manual-ticket rule in [triggers.md](triggers.md)
  reads it, and it stores only the `cap:` note and the forwards attached to it.

## Config shape

```toml
[[chat]]
id = 123456789
label = "Anh Minh"
kind = "dm"
triggers = ["dm_all"]
profile = "dm"

[[chat]]
id = -1001234567890
label = "Ops HCM"
kind = "forum"
topics = [1, 4521]          # 1 is the General topic
triggers = ["mention", "reply_to_me", "keyword"]
keywords = ["deploy", "hoàn tiền"]
profile = "ops"
mute_until = "2026-10-01"
```

Validation on load, rejecting the whole file on any error:

- Unknown trigger or profile name.
- Duplicate id.
- `topics` on a chat that is not a forum.
- An id the session has never seen gives a warning, not an error: the daemon cannot resolve its
  access hash yet, but it will once a message arrives.

`captain chats` lists recent dialogs with their ids and kinds, so building the file does not
require reading raw JSON.

## Runtime switches

| Switch | Where | Effect |
|---|---|---|
| `captain pause` / `resume` | daemon state, persisted in the store | No new tickets. Ingest and storage continue, so context stays complete when resumed. |
| `mute_until` | per entry | Same as pause, for one chat |
| Removing an entry | `scope.toml` + `scope reload` | Stops storing. Stored rows age out through retention. |

Pause keeps storing because a resumed pipeline with a hole in its history pulls worse context,
and the data is already covered by the scope decision.

## Topics

TELEGRAM: in a forum, a message in topic T has `reply_to.forum_topic = true` and either
`reply_to_msg_id = T` (a plain message in the topic) or `reply_to_top_id = T` (a reply to
another message inside T). Messages in the General topic have no reply header. The gate
derives `topic` from these and matches it against `topics`. Verified live in P0.
