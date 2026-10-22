# hub-bridge

`hub-bridge` turns `RemoteEvent`s from any number of `NetworkAdapter`s into posts in Telegram forum topics, and sends replies, reactions, edits, and deletes typed in a topic back to the source network. It does not open a socket. Tests pass a fake adapter, a fake sink, an in-memory store, and a manual clock.

## Public API

```text
Bridge::new(config, adapters, sink, store, clock)
bridge.run(shutdown).await
bridge.handle_remote(event).await
bridge.handle_topic_message(incoming).await
bridge.health()
```

`Bridge` is generic over the sink, the store, and the clock. Adapters are `AdapterHandle` trait objects. `NetworkAdapter` returns `impl Future`, which is not object-safe, so `AdapterHandle` boxes those futures. Every `NetworkAdapter` implements it.

`Clock::sleep_until` is how a rate-limit pause waits. `ManualClock::advance` wakes waiters in tests. `SystemClock` is the wall clock for a process.

`BridgeStore` is the mapping contract: topics, messages, display names, failures, and retry marks. `MemoryStore` implements it in memory. Cloning the store shares the maps, so a new `Bridge` with the same store is a restart. `SqliteStore` implements the same trait over `hub_store::SharedHub`. Reopening the file restores topic and message rows. `message_map` is still unique on `(network, network message id)`, so two conversations cannot share one network id.

`handle_remote` and `handle_topic_message` return `Err` when the bridge has shut down or the store write failed. A sink or adapter error during delivery is written to `failed_events` and the call returns `Ok`. The lane then takes the next job.

## TopicIncoming

`hub-tgbot` produces `TopicIncoming`. This crate does not import `hub-tgbot`. JSON is internally tagged with `type` and `snake_case` names, the same shape as `RemoteEvent`. `TopicId` and `TgMessageId` are bare numbers.

```json
{"type":"message","topic":1,"tg_message":10,"text":"hi","attachments":[],"reply_to":null}
{"type":"reaction","topic":1,"tg_message":10,"emoji":"👍","added":true}
{"type":"edit","topic":1,"tg_message":10,"text":"new"}
{"type":"unsend","topic":1,"tg_message":10}
```

`reply_to` and `emoji` use the Telegram values. The bridge looks them up in `message_map` and, for Zalo reactions, in the allow-list below.

## Startup and health

`run` loads topic rows, then subscribes to adapters. Shutdown stops the readers and waits until every lane is idle. A rate-limit wait is cancelled on shutdown so the drain stays bounded. An in-flight send is allowed to finish.

`health` returns `ModuleHealth` for module `bridge`. `detail` is one line:

```text
queue_depth={n} retries_24h={n} rate_limited=false
queue_depth={n} retries_24h={n} rate_limited=true retry_after_secs={n}
```

`Unknown` before `run`, `Ok` while running with an empty failure count, `Degraded` while rate limited or after a recorded failure, `Down` after `run` returns.

## Ordering, topics, and delivery

One conversation has one queue and one topic. Different queues run together, up to `TOPIC_CONCURRENCY` (8). A failed job does not stick the lane.

The dedupe key is the conversation plus the network message id. The same id in two conversations is delivered twice. The message row is inserted only after the whole send plan succeeds, and the first insert wins, so a media group stores only the first Telegram id. `send_media` has no reply slot, so a reply on a media message is dropped and the text moves to the first caption.

`TopicGone` drops the mapping before a replacement is created. A failed `create_topic` therefore leaves no row. The send is retried once (`TOPIC_RECREATE_LIMIT`). A second `TopicGone` is stored and the lane moves on. `TopicGone` on delete counts as success. `TopicGone` on a reaction drops the mapping and does not open a topic only to react.

`RateLimited` pauses every sink call for `retry_after_secs` and retries until `SINK_MAX_ATTEMPTS` (3, including the first). Other errors go to `failed_events` with a clipped reason and a retryable flag. Nothing in the library panics on those errors.

An unknown reply, reaction, unsend, or edit becomes a plain message. It is not a failure row.

There is no edit method on `TopicSink` or `NetworkAdapter`. A known edit is a new message that replies to the mapped id. The original row stays.

`ChatInfo` stores the raw title and, when the directory already had one, the avatar URL. The rendered title is `{network emoji} {kind mark} {name}`, whitespace collapsed, clipped to 128 UTF-16 units with `…`. The network emoji is 🔵 for Zalo, 🟣 for Messenger, and ✈️ for Telegram. The kind mark is 👤 for a person and 👥 for a group. The stored title has neither mark, so a recreate does not prefix it twice. `rename_topic` runs when that rendered title differs from the last title this process set, at most once per conversation per 24 hours (`RENAME_INTERVAL_SECS`), through the same send queue as other sink calls. A sink that returns `Unsupported` is skipped. `TopicGone` on rename drops the mapping and creates a replacement once. An avatar card is one photo, file name `avatar-card`, posted once per conversation when `avatar_card` is true. A pin failure is logged and ignored.

Text posted to Telegram is HTML with `&`, `<`, and `>` escaped. Quotes are left as-is. The body is clipped to `TEXT_MAX_UTF16` with `…`. Captions are plain text, clipped to `CAPTION_MAX_UTF16`.

## Reactions and mentions

`TELEGRAM_REACTIONS` is the 73 emoji a bot may pass to `setMessageReaction`. Zalo icon codes map onto that set. An icon or emoji outside the set is skipped for an add. A removal clears the Telegram reaction. The reverse map turns a Telegram emoji into a Zalo icon. Messenger and Telegram keep the emoji when it is in the set.

Display names are cached from senders and `Mention`s. Groups are scoped to the conversation. Direct messages are scoped to the network. Lookup folds case, strips diacritics, and maps `đ` to `d`. `@Name` in a topic message takes the longest cache hit, up to 5 words. An `@` inside a word is ignored. `OutgoingMessage` has no mention field, so the resolved id is used for the cache lookup and the text is forwarded as typed.

Session changes and friend requests post an italic notice to `BridgeConfig::ops_topic` when that topic is set. They are logged and dropped when it is not. The ops topic is not recreated on `TopicGone`.

## hub-store

`store_sqlite` is not in this crate. `hub-store` can insert topic and message rows, but the bridge also needs to read a message back by conversation and id, drop a topic, and remember a display name. None of those reads exist yet. `message_map` is unique on `(network, network_message_id)`, so it cannot represent the same id in two conversations. `map_message` replaces the row on conflict, which is the opposite of first-write-wins. `Hub` holds a `rusqlite::Connection`, which is not `Sync`, and several methods take `&mut self`. When those gaps close, an adapter can implement `BridgeStore` over `hub-store` without changing the call sites above.
