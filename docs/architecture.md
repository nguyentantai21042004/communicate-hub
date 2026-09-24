# Architecture

## Layers

```
captain-cli ──┬──> captain-format ────> captain-core
              ├──> captain-protocol ──┬─> captain-core
              │                       └─> captain-vault
              └──> captain-vault
```

`captain-core` defines what a message is, independent of any Telegram library. Everything else
converts to or from it.

Layer 1, `captain-protocol`, is the only crate that knows grammers exists. It implements the
`captain_core::Protocol` trait, so a TDLib adapter or a test double could replace it without
touching callers.

Layer 2, `captain-format`, turns what a person types into `FormattedText`, which is plain text
plus a list of styled ranges. That is exactly how MTProto represents styling, so layer 1
converts entities one to one and never parses markup.

## Inbound

1. grammers' update stream yields a raw `tl::enums::Update`.
2. `inbound::RawPayload::capture` serializes it to JSON before anything else touches it.
3. `Update::from_raw` wraps it, and `inbound::to_event` builds the normalized `EventKind`.
4. The caller gets an `InboundEvent` with both. When the normalized view is missing a field,
   the raw payload still has it.

## Outbound

`OutgoingMessage::validate` runs first and checks length, entity ranges and album rules, with
no network. Then:

| Content | Request |
|---|---|
| Text | `messages.sendMessage` through grammers |
| One photo or file | `messages.sendMedia` through grammers |
| Album of 2 to 10 | `upload.saveFilePart` per item, `messages.uploadMedia` per item, then a raw `messages.sendMultiMedia` |

Albums bypass grammers' `send_album` because it hardcodes `silent` and `invert_media` to false.

Telegram has no "HD" flag. The apps' HD toggle means the client uploads the image without
shrinking it, and the server keeps up to 2560 px. `PhotoQuality::Hd` does that.
`PhotoQuality::Original` sends the exact file as a document.

## Session storage

grammers asks its `Session` trait for everything it persists. `KeychainSession` splits that by
sensitivity:

| Data | Where | When written |
|---|---|---|
| Auth keys, home datacenter | Keychain entry `<profile>/mtproto_auth` | immediately on change |
| Datacenter addresses | state file | immediately on change |
| Peer cache, update position | state file | every 30 s and on close |

The unit tests in `session.rs` check that an auth key never reaches the state file.

## Known gaps

- Only plain text in layer 2.
- Videos, voice notes and stickers are received and downloadable, but sent only as generic files.
- No per-chat filter or on/off switch yet. `listen` prints every update.
- Nothing has run against a live account yet. The first `captain auth login` is the real test.

## Dependency pins

grammers is pinned to `=0.10.0` because pre-1.0 releases break between minors.
`glass_pumpkin` is held at `2.0.0-rc0` in `Cargo.lock`: `grammers-crypto 0.10.0` does not
compile against `2.0.0-rc1`, which changed `safe_prime::check`. Do not run a bare
`cargo update`; update named packages and rerun `make check`.
