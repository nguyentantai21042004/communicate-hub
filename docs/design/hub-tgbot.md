# hub-tgbot

`hub-tgbot` is the Bot API side of the bridge. [`BotClient`](../../crates/hub-tgbot/src/client.rs) implements [`hub_core::TopicSink`](../../crates/hub-core/src/sink.rs) for one forum supergroup, and it long-polls that group for messages and reactions. The token comes from the caller. This crate does not read the environment or a file for it.

The HTTP client is `frankenstein` 0.52 with the `client-reqwest` feature (`frankenstein` 0.52.1, Bot API 10.3). Every method the adapter uses is on that client, including `setMessageReaction`. Nothing is hand-rolled with `reqwest`.

Tests bind a fake on `127.0.0.1`. [`BotConfig::api_base_url`](../../crates/hub-tgbot/src/config.rs) is the origin only. The client appends `/bot` and the token. `BotToken` and `BotClient` debug-print `<redacted>`.

## What the bridge can call

`TopicSink`: `create_topic`, `close_topic`, `send_text` (HTML, passed through), `send_media`, `set_reaction` (`None` clears), `delete_message`, `rename_topic` (`editForumTopic`). The trait's default `rename_topic` returns `Unsupported`. This client overrides it.

`TopicSink::create_topic` loads `getForumTopicIconStickers` once per process. The second title token `👥` selects the group icon and anything else selects the person icon. The preference lists are 👤 🙂 💙 and 👥 💬 🟢. A match uses `icon_custom_emoji_id` and no color. A miss, or a failed fetch, uses blue for a person and green for a group. The inherent `create_topic` still sends no icon.

On `BotClient` only: `create_topic_with` (one of the six `icon_color` values, optional custom emoji), `edit_forum_topic`, `pin_message`, `reopen_topic`, `edit_message_text`, `send_media_group`, `preflight`, `run_updates`. A photo whose `file_name` is `avatar-card` is pinned with notifications off. A pin failure is logged and the message id is still returned.

`send_media` picks the method from `AttachmentKind`: photo, video, audio, voice, document, animation. A static sticker is sent with `sendPhoto`. An animated sticker (`application/x-tgsticker`, `.tgs`) or a video sticker (`video/webm`, `.webm`) is sent with `sendDocument`. This crate does not call `sendSticker`. The older `chat-bridges` sender calls `sendSticker` and falls back to a document. The task for this crate is the photo/document split.

A URL is passed through. A local path is multipart. Two or more photos use `sendMediaGroup` in batches of 10. The caption is plain text and goes on the first photo only. One photo uses `sendPhoto`.

Location, poll, bank card, and contact return `Error::Permanent`. The bridge should turn those into text before it calls the sink.

## Errors

`Error::RateLimited` and `Error::TopicGone` match `hub_core::Error`. The trait has no variant for the rest, so `From<Error> for hub_core::Error` folds them into `Adapter` with a stable `detail` prefix: `too_large`, `bot lacks rights`, `transient`, `permanent`. `adapter_detail` carries a status or a size. It does not carry the token, the message text, or the raw Telegram description.

`TooLarge` uses the named caps in `limits`: 20 MiB download, 50 MiB upload. A known `size_bytes` over the cap fails before any HTTP call, so the bridge can send a text note. A URL with no size is not downloaded here.

HTTP 429 becomes `RateLimited`. `retry_after` comes from the response, or 30 seconds when it is missing. `message thread not found`, `TOPIC_DELETED`, and `TOPIC_CLOSED` are `TopicGone`. HTTP 403, or a description about missing rights, is `LacksRights`. HTTP 408 and 5xx are `Transient`. Other 4xx values are `Permanent`. A connection failure with no status is `Transient`.

## Queue

Five send calls in flight (`REQUEST_CONCURRENCY`). A 429 pauses every caller, including the long poll, for `retry_after` plus one second, then retries the same call. After five retries the error is returned. `getUpdates` uses its own HTTP client and does not take a send slot, so a 25 second poll does not block the five sends.

## Incoming

`hub-bridge` now defines its own `TopicIncoming`: `Message` and `Reaction` with `hub_core::Attachment` and a required topic, plus `Edit` and `Unsend`. This crate's enum is not that type. It is `Message(IncomingMessage)` and `Reaction(IncomingReaction)`. `Edit` and `Unsend` are not produced: `getUpdates` asks only for `message` and `message_reaction`.

`getUpdates` allows `message` and `message_reaction`, with a 25 second timeout. The caller implements `OffsetStore`. This crate does not write the offset itself. The next offset is `update_id + 1`, saved after the events for that update are queued, including updates that were ignored. Messages from this bot and from other chats are ignored. The General topic has no `message_thread_id`; it is reported as topic `1`.

`message_reaction` has no thread id. `IncomingReaction.topic` is `None`. The bridge correlates the reaction by `TgMessageId`. Custom emoji and paid reactions are dropped. Each added or removed emoji is its own event.

`IncomingAttachment` carries `file_id`. `hub_core::Attachment` cannot, because that type is the outbound file.

## Preflight

`preflight` calls `getMe`, `getChat`, and `getChatMember`. The report says whether the chat is a forum supergroup and whether the bot can manage topics, send, delete, and react. A failing chat is still `Ok` with `NotSupergroup`, `ForumDisabled`, or `MissingRights`. An HTTP failure is `Err`.

## Gaps

- Multipart upload uses the path's file name. `Attachment.file_name` is not sent as the upload name.
- A later album batch can fail after an earlier batch was posted.
- A reaction cannot be routed to a topic from the update alone.
- `TopicSink` callers see `too_large` as `Adapter.detail`. The typed `TooLarge` stays on `BotClient` methods.
