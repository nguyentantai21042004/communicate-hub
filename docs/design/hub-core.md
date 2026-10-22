# hub-core

`hub-core` is the vocabulary the hub crates share. It defines conversations, events, and two traits. It does no I/O, and it never sees zca-js types.

## Ids

A conversation id is `network:kind:external_id`.

Networks on the wire are `telegram`, `zalo`, and `messenger`. Kinds are `dm` and `group`. The external id belongs to that network and may itself contain colons, so `zalo:group:a:b` has external id `a:b`.

`ConversationId` displays that string, parses it back, and serializes as that string. Parsing rejects a missing part, an empty part, an unknown network, and an unknown kind. The match is case sensitive.

`MessageRef` is a conversation id plus the source message id as a string. `TopicId` is Telegram's `message_thread_id`. `TgMessageId` is a Bot API `message_id` in the bridge supergroup.

## Events

`RemoteEvent` is a serde enum tagged with `type`. Variant names are snake_case. `Message` and `Edit` are newtype variants of structs, so those fields sit next to `type` in the JSON. `SessionChanged` is a struct variant with a `state` field, so the JSON is `{"type":"session_changed","state":"valid"}`.

`SessionState` is `valid`, `expired`, `logged_out`, or `connecting`. `ModuleHealth` is a dashboard snapshot with `ModuleState`: `ok`, `degraded`, `down`, `unknown`.

`OutgoingMessage` travels from Telegram to a source network. Its text is plain. `TopicSink::send_text` takes Telegram HTML instead, and the sink passes that HTML through.

`AttachmentSource::Url` serializes as `{"url":"..."}`. `AttachmentSource::LocalPath` serializes as `{"local_path":"..."}`. `mime` and `file_name` are optional because stickers, locations, and polls often have neither.

## Traits

`NetworkAdapter` is the source side. `hub-zalo` implements it. A later Messenger crate implements the same trait. `next_event` returns `Ok(None)` when the adapter shuts down cleanly. `send` returns the `MessageRef` the network assigned.

`TopicSink` is the Telegram forum side. `hub-tgbot` implements it. `create_topic` returns a `TopicId`. `send_text` and `send_media` return the `TgMessageId` they posted. `set_reaction` clears the reaction when `emoji` is `None`.

`hub-bridge` is generic over both traits and can run against fakes. `hub-store` and `hub-web` use the types and do not implement the traits.

Every method takes `&self` and returns `impl Future + Send`. Implement a method with `async fn`. The `Send` bound is what keeps a generic bridge task spawnable on Tokio. `async fn` in the trait itself does not promise that.

Both traits return `hub_core::Result`. The variants a bridge matches are `RateLimited`, which carries `retry_after_secs`, and `TopicGone`. Any other adapter failure is `Adapter`. `Adapter.detail` is logged, so it holds an id or a status, never message text or a token.

## Limits

`hub_core::limits` re-exports `TEXT_MAX_UTF16`, `CAPTION_MAX_UTF16`, and `CAPTION_PREMIUM_MAX_UTF16` from `captain_core::limits`. The bridge checks the text limit before `send_text` and a caption limit before `send_media`.
