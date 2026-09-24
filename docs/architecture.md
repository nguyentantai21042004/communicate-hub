# Architecture

## Layers

```
captain-cli ──┬──> captain-format ────> captain-core
              ├──> captain-protocol ──┬─> captain-core
              │                       └─> captain-vault
              ├──> captain-inbox ─────> captain-core
              ├──> captain-store ──┬──> captain-core
              │                    └──> captain-inbox
              ├──> captain-ipc ───────> captain-core
              └──> captain-vault
```

`captain-inbox` is the pure inbox logic: scope, triggers, coalescing, triage and the report
math. It does no I/O and takes time as an argument. `captain-store` is the SQLite store and
depends on `captain-inbox` for its types. `captain-ipc` frames the daemon's socket protocol.
`docs/design/README.md` explains each of them.

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

grammers keeps the update position in memory and gives it to the session only when asked.
`Connection::next_event` asks after every update, so the 30 s flush writes a current position
and a crash replays at most about 30 s of updates. The daemon skips replayed messages that are
already in the store.

The unit tests in `session.rs` check that an auth key never reaches the state file.

## Known gaps

- Only plain text in layer 2.
- Videos, voice notes and stickers are received and downloadable, but sent only as generic files.
- Nothing has run against a live account yet. The first `captain auth login` is the real test.
- P1 of the inbox is built: daemon, store, scope, triggers, tickets, metrics and the monitoring
  UI. The Claude runner, context tools and drafts (P2) are designed in `docs/design/` and not
  built; they wait on the API terms decision in `docs/design/risks.md`.

## Daemon

`captain daemon` owns the session. One task owns the `Connection` and only calls
`next_event`, because grammers' update stream loses updates when a read is cancelled midway.
Sends and fetches go through `captain_protocol::Handle`, a cloneable client that the other
tasks share. `docs/design/daemon.md` has the task list and failure handling.

## Build size

`grammers-tl-types` compiles the whole TL schema, so one clean `make check` leaves about 1 GB
in `target/`. Two things keep it from growing past that:

- The dev profile in `Cargo.toml` builds dependencies without debuginfo and workspace crates
  with line tables only. Before this, macOS kept one object file per codegen unit for every
  rebuild, and a clean build was 1.35 GB instead of 0.99 GB.
- `scripts/target-gc.sh` caps `target/` at 2 GB. `make check`, `make build`, and the Claude
  Code Stop hook in `.claude/settings.json` all call it. Over the cap it deletes incremental
  caches first, and runs `cargo clean` only if that is not enough. It does nothing while a
  build is using the directory. Set `CAPTAIN_TARGET_CAP_MB` to change the cap.

## Dependency pins

grammers is pinned to `=0.10.0` because pre-1.0 releases break between minors.
`glass_pumpkin` is held at `2.0.0-rc0` in `Cargo.lock`: `grammers-crypto 0.10.0` does not
compile against `2.0.0-rc1`, which changed `safe_prime::check`. Do not run a bare
`cargo update`; update named packages and rerun `make check`.
