# Hub store

## Responsibility

One SQLite file holds Captain's inbox and the hub tables the dashboard reads. Telegram traffic
is already in Captain's tables. Zalo, and later Messenger, show up as conversations and hour
counters. The dashboard never needs a second database.

Captain-store owns the file and `PRAGMA user_version`. Hub-store opens that same file and adds
its own tables. It does not store message text.

## Migration

Hub versions live in `hub_schema(version)`. `migrate` does not write `user_version`.

The migration is additive and idempotent. Each version runs once. A statement that has shipped
is not edited; the next change is a new version block above the commit of `hub_schema`. A file
whose version is newer than this build is refused, and its rows are left as they were.

`Hub::open` calls `captain_store::Store::open` first, so a Captain migration still runs when
the file needs one. That call is the only writer of `user_version`. An in-memory hub copies
Captain's schema, including the `user_version` Captain would have set, and then applies the
hub migration without touching the pragma again.

## Tables

| Table | Key | What it stores | Retention |
|---|---|---|---|
| `conversations` | network, kind, external id | title, topic id, muted, first seen, last activity, tags | kept |
| `message_counters` | UTC day, hour, conversation, direction, content kind | count | kept |
| `topic_map` | conversation id | Telegram topic id, created at | kept |
| `message_map` | network, network message id | Telegram message id, created at | newest 20 000 rows |
| `bridge_events` | id | time, module, level, kind, short detail | `prune_events`, default 30 days |
| `module_events` | id | same shape as bridge events | same prune |
| `failed_events` | id | conversation, reason, retryable, created at, resolved at | until the caller deletes them |
| `module_health` | module | state, detail, updated at | latest row per module |
| `hub_schema` | | version | one row |

`conversations.topic_id` is unique when set. `source_of_chat` reads it.

Direction is `in` or `out`. Content kind is `text`, `photo`, `video`, `voice`, `file`,
`sticker`, `poll`, `bank_card`, or `other`. The kind is a tag. The bytes are not stored.

## Bridge supergroup

The bridge forum's chat id is the `settings` row `bridge_supergroup_id`. It is not a constant
in the binary. `source_of_chat` treats every other chat id as Telegram. When the chat id is
the bridge supergroup, the source is the conversation whose topic id matches the ticket, and
a missing topic is an error.

## Privacy

No hub column stores a message body. The forbidden names are text, body, caption, message,
quote, and content. `CountedMessage` has a conversation id, a timestamp, a direction, and a
content kind. `RemoteEvent` from `hub-core` can carry a body; this crate does not read that
field.

Event detail and failure reasons are cut at 240 Unicode scalars. Titles and tags are
conversation metadata, not message bodies. Captain's `messages` table, including its text
column, is not updated by hub writes.

## Schema 2

`HUB_SCHEMA_VERSION` is 2. Version 1 is unchanged. Version 2 adds `user_names`,
`bridge_retries`, an index on `message_map.telegram_message_id`, and a nullable
`message_map.conversation_id`. The column is added only when it is missing. A file newer than
this build is refused before any of that runs.

`user_names` stores a display name for one user inside a conversation scope. `bridge_retries`
stores the Unix time of a delivery retry so a restarted bridge can rebuild the 24 hour count.

`map_message` still replaces the Telegram id and leaves `conversation_id` null. Those rows are
legacy. `message_link_for_telegram` returns nothing for them, so a restart will not restore
that link. `insert_message_link` is the bridge write. The first write wins, and it sets
`conversation_id`. A later write for the same network message id keeps the first Telegram id.
`Ok(false)` means another conversation already owns that network message id.

`message_map` is still unique on `(network, network_message_id)`. Two conversations cannot
share one network id. `SqliteStore` turns that `Ok(false)` into a store error.

`upsert_conversation` with an empty topic clears the topic. A title change goes through
`ensure_conversation`, which leaves the topic in place.

## Shared handle

`rusqlite::Connection` is `Send` and not `Sync`, so `Hub` cannot cross an await on its own.
`SharedHub` is `Arc<Mutex<Hub>>`. One connection, one writer. A poisoned lock is recovered
because the rows are still there. Callers on the async runtime run the locked call inside
`spawn_blocking`. A worker thread would only put a channel in front of the same mutex. The
daemon store already uses this shape.
