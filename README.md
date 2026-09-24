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

Phase 1 of the inbox is built: a daemon that watches the chats in `scope.toml`, turns mentions,
replies and DMs into tickets, measures how they end, and shows it all on a local web page. It
drafts nothing yet. Claude drafts are phase 2, which waits on the API terms question below.

| Crate | Layer | What it does |
|---|---|---|
| `captain-core` | shared types | Messages, entities, media, Telegram limits. No I/O. |
| `captain-protocol` | layer 1 | MTProto in and out, login, session storage. |
| `captain-format` | layer 2 | Authoring syntax to styled text. Plain text only for now. |
| `captain-vault` | credentials | Keychain secrets and the local state directory. |
| `captain-inbox` | inbox logic | Scope, triggers, coalescing, triage, report math. No I/O. |
| `captain-store` | inbox store | SQLite: scoped messages, tickets, metric events. |
| `captain-ipc` | daemon socket | The JSON-lines protocol between `captain` and the daemon. |
| `captain-cli` | app | The `captain` command, the daemon and the web UI. |

`docs/architecture.md` explains how the pieces fit. `docs/CONVENTIONS.md` has the code rules.
`docs/design/` describes the planned inbox: a daemon that turns mentions and DMs into tickets,
and Claude drafts that you review in the Telegram app before sending.

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

## Running the inbox

List recent chats to find their ids, then write `scope.toml` in the state directory
(`docs/design/scope.md` has the format):

```sh
captain chats --limit 50 | jq '.[] | {id: .chat.id, name: .chat.name, forum}'
captain scope check          # parses scope.toml and policy.toml, prints what is watched
captain daemon               # in the foreground; Ctrl-C stops it
```

To keep it running without thinking about it, install it as a LaunchAgent. It starts at every
login and restarts after a crash:

```sh
make build && scripts/install-agent.sh   # repeat after a rebuild you want the daemon to use
```

The script copies the binary to `~/.local/bin/captain`, checks the login in the foreground so
macOS asks for Keychain access while you are there, and starts the agent. Log in with that
installed binary (`~/.local/bin/captain auth init`, then `auth login`), so Keychain trusts the
program the daemon runs.

While the daemon runs, the other commands talk to it instead of opening their own session:

```sh
captain status
captain pause                # stop creating tickets; messages are still stored
captain resume
captain tickets --days 1 | jq .
captain ticket label 42 want # want, noise, skip or clear
captain report --text        # the phase 1 metrics and decision rules
captain ui --open            # the monitoring page on 127.0.0.1:8787
```

`docs/design/monitor.md` describes the page and how it is locked down. To look at it without an
account, seed a throwaway store: `captain --state-dir /tmp/captain-demo dev seed-demo`, then
`captain --state-dir /tmp/captain-demo ui serve`.

## Where things are stored

The Keychain service `dev.tantai.telegram-captain` holds the API hash and the MTProto auth keys.
An auth key is the logged-in account, so it never touches disk. Everything else the session
tracks goes to `~/Library/Application Support/telegram-captain/<profile>/mtproto_state.json`
with mode `0600`: datacenter addresses, the peer cache and the update position.

`captain auth logout` ends the session on Telegram and deletes both. `--profile work` keeps a
second account fully separate.

## Rules worth knowing

- Run one process per session. Two processes sharing one auth key confuse the server, and
  Telegram may revoke the session. The daemon and direct commands enforce this with a lock
  file, but the Python prototype in `prototype/python/` does not know about it: never run
  it next to `captain`.
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
