# telegram-captain

Receive the Telegram messages that reach your own account, and reply as yourself, from a small
Rust program that can run on an idle Mac.

It logs in as a user account over MTProto, the same protocol the official apps use, through the
[grammers](https://codeberg.org/Lonami/grammers) library. Bots cannot read a person's direct
messages, so a bot token would not work here.

## Status

The protocol layer works end to end in code and passes its unit tests. It has not yet been run
against a live account. The format layer, which will turn Markdown into bold, italic and links,
is a placeholder that sends text as written.

| Crate | Layer | What it does |
|---|---|---|
| `captain-core` | shared types | Messages, entities, media, Telegram limits. No I/O. |
| `captain-protocol` | layer 1 | MTProto in and out, login, session storage. |
| `captain-format` | layer 2 | Authoring syntax to styled text. Plain text only for now. |
| `captain-vault` | credentials | Keychain secrets and the local state directory. |
| `captain-cli` | app | The `captain` command. |

`docs/architecture.md` explains how the pieces fit. `docs/CONVENTIONS.md` has the code rules.

## Quick start

You need an API id and hash from <https://my.telegram.org/apps>. They identify the app. The
account itself is the session that `auth login` creates.

```sh
make build                                   # target/release/captain
export PATH="$PWD/target/release:$PATH"

captain auth init        # reads TELEGRAM_API_ID / TELEGRAM_API_HASH, or prompts
captain auth login       # phone number, login code, then the two-step password if set
captain auth status
```

Watch what arrives. Each update is one JSON line on stdout with the normalized view and the raw
TL payload side by side.

```sh
captain listen | jq .
```

Send as yourself:

```sh
captain send --to me --text "test"
captain send --to @someone --text "caption on top" --photo a.jpg --caption-above
captain send --to @someone --photo a.jpg --photo b.jpg --text "album" --silent
captain send --to @someone --photo scan.png --original       # exact bytes, sent as a file
captain send --to @someone --photo s.jpg --spoiler
captain send --to -1001234567890 --text "ok" --reply-to 42
captain download --chat 123456789 --message 42 --out ./photo.jpg
```

A numeric `--to` works only for chats the account has already seen, because Telegram needs the
access hash that comes with a received message or a username lookup.

## Where things are stored

The Keychain service `dev.tantai.telegram-captain` holds the API hash and the MTProto auth keys.
An auth key is the logged-in account, so it never touches disk. Everything else the session
tracks goes to `~/Library/Application Support/telegram-captain/<profile>/mtproto_state.json`
with mode `0600`: datacenter addresses, the peer cache and the update position.

`captain auth logout` ends the session on Telegram and deletes both. `--profile work` keeps a
second account fully separate.

## Rules worth knowing

- Run one process per session. Two processes sharing one auth key confuse the server, and
  Telegram may revoke the session.
- Telegram's [API terms](https://core.telegram.org/api/terms) require that the account owner
  knows what the app does. Section 1.5 restricts using Telegram data to build or run AI models.
  Read it before feeding messages into an LLM.
- The app reports itself as `Captain (Rust)` in the account's device list, because the terms
  forbid third-party apps from using "Telegram" in their name.

## Development

```sh
make check    # rustfmt, clippy -D warnings, tests, comment-tag lint
```

`refs/` holds read-only clones of the libraries this was built from; see `refs/README.md`.
`prototype/python/` is the Telethon demo used to explore payloads before the Rust rewrite.
