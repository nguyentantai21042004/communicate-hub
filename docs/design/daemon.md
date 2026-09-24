# Daemon and IPC

## Responsibility

`captain daemon` is the only process that opens the Telegram session. It receives updates,
runs the inbox pipeline, keeps the store, serves the socket, and in P2 runs Claude. Every other
command is a client.

## Why one process

Two processes using one auth key make the server see two clients with the same key and
diverging update state. In research this got a Telethon session revoked. Before the daemon,
the CLI opened its own `Connection` per command, so `captain send` while `captain listen` ran
was unsafe. Now every command goes through the daemon when it runs, and takes the session lock
when it does not.

## Exclusive ownership

- The daemon takes an exclusive `flock` on `<state dir>/session.lock` before opening the
  session, and holds it until exit.
- Direct CLI commands (`auth login`, and `send` or `download` when no daemon runs) take the same
  lock without waiting. If it is held they fail with "daemon is running, use it" instead of
  opening a second connection.
- A lock held by a dead process is released by the kernel, so there is no stale-lock cleanup.

State dir is the existing `~/Library/Application Support/telegram-captain/<profile>/`.

## Tasks inside the process

| Task | Owns | Notes |
|---|---|---|
| Connection | `captain_protocol::Connection` | Existing code. Yields `InboundEvent`s. |
| Pipeline | scope gate, triggers, tickets | One task, events handled in arrival order |
| Store | SQLite connection | Behind a mutex; every call runs on a blocking thread, because SQLite calls block. |
| IPC server | Unix socket | One task per client connection |
| Web UI | `127.0.0.1:8787` | See [monitor.md](monitor.md) |
| Pipeline timer | debounce deadlines, manual note windows | A one-second tick inside the pipeline task |
| Jobs | heartbeat, ticket resolver, retention, store size, catch-up count | One task with a timer per job |
| Runner (P2) | `claude -p` child processes | Queue of one |

Each task has an owner that awaits or cancels it on shutdown, as `docs/CONVENTIONS.md` requires.
Shutdown order: stop reading updates, stop the other tasks, remove the socket, record the end
of the run, then `Connection::close`, which saves the update position and session state. Open
bursts are dropped; their messages are already stored.

## IPC

- Socket: `<state dir>/captain.sock`, mode `0600`, directory `0700`.
- The server checks the peer uid with `getpeereid` and rejects any uid other than its own.
- Protocol: JSON lines. One request object, one response object, same `id`. `subscribe` keeps
  the connection open and streams events.
- The only TCP port is the loopback web UI in [monitor.md](monitor.md). If the runner ever
  moves to another machine, that becomes an SSH-forwarded socket, decided then.

| Method | Caller | Purpose |
|---|---|---|
| `status` | CLI, UI | Connection state, paused flag, open bursts, last day's counts |
| `pause`, `resume` | CLI, UI | Global switch for ticket creation |
| `subscribe` | `captain listen` | Stream of events, as `listen` prints them |
| `send`, `download`, `chats` | CLI | Commands that need Telegram, through the daemon |
| `tickets`, `ticket`, `label`, `new_ticket` | CLI, UI | Inspect, label, create manually |
| `scope_reload` | CLI, UI | Reread `scope.toml` and `policy.toml` |
| `report` | CLI, UI | The phase 1 report |
| `context.*`, `draft.save`, `ticket.decline` | `captain mcp` (P2) | Tools in [context-tools.md](context-tools.md) |

`captain mcp` is a stdio MCP server that the runner starts. It holds no session; it forwards
tool calls to the daemon over this socket, tagged with the ticket id so the daemon can apply
that ticket's budget.

## Running under launchd

- launchd agent `dev.tantai.telegram-captain.<profile>` in `~/Library/LaunchAgents`, written
  by `scripts/install-agent.sh`, with `RunAtLoad`, `KeepAlive` on unsuccessful exit and a
  `ThrottleInterval` so a crash loop does not hammer Telegram. A LaunchAgent, not a
  LaunchDaemon, because the Keychain is per user and unlocked at login. It starts at each
  login, so after a restart it runs once the owner has logged in.
- launchd runs a copy at `~/.local/bin/captain`, not `target/release/captain`, so a rebuild
  does not replace the binary of a running daemon. The script updates the copy on request.
- A rebuilt binary is a new program to Keychain, which then asks before handing out the
  secrets. The script runs `captain auth status` in the foreground before starting the agent,
  so that prompt appears while the owner is at the machine and not behind a background daemon.
- Without API credentials or a login the daemon logs why and exits with status 0, so launchd
  does not restart it in a loop. Other start failures, such as no network right after login,
  exit with an error and launchd retries after the throttle interval.
- Logs go to `~/Library/Logs/telegram-captain/<profile>.<date>.log`, one file per day, the last
  14 kept (`captain daemon --log-dir`). stderr goes to `<profile>.stderr.log` and only catches
  what bypasses logging, such as a panic. Logs carry ids, counts and types. Message text only
  at `trace` level.
- Sleep: while the Mac sleeps the daemon is offline. On wake the dead connection shows up as a
  gap, grammers reconnects to fill it with `updates.getDifference`, and tickets older than the
  stale threshold go to the digest instead of producing a late draft. On a machine meant to
  stay up, set `pmset -c sleep 0` while on power.

## Catching up

Telegram pushes updates over the open connection; nothing polls. Each update carries a
sequence number, and grammers fills any hole with `updates.getDifference`, or
`updates.getChannelDifference` for a supergroup, 100 messages per call.

- On start the daemon opens the stream with catch-up on, so it asks for everything since the
  saved position. The position is saved after every update and written by the 30 s flush.
- Before the first read it lists all dialogs once, because a supergroup gap can only be filled
  with the chat's access hash, and grammers caches hashes from that call but not from updates.
- A message that is already in the store is skipped, so a replay after a crash makes no second
  ticket.
- A gap Telegram considers too long comes back as `differenceTooLong`, and grammers jumps to
  the present. Those messages are still in the chat history but never reach the pipeline.

## Failure handling

| Failure | Behaviour |
|---|---|
| Network drop | grammers reconnects. A failed read is logged, recorded as an API error, and retried after 5 s. |
| `updates.differenceTooLong` or `updateChannelTooLong` | Some updates are lost for good. grammers resyncs; P1 only sees it as missing messages in recall (M6). P2 records a coverage gap for the affected chats so context tools use the API for that range. |
| `FLOOD_WAIT_X` | Wait X seconds on that method only, record it, never retry sooner. |
| Auth key revoked (401) | Stop reading updates, keep the socket and UI up with status `unauthorized`. Do not let launchd restart into a login loop. |
| Store write error | P1 logs it at error level for each event and carries on, because nothing acts on a ticket yet. The P2 runner must stop instead: a pipeline that cannot remember what it did must not act. |

## What Tài sees on other devices

- The device list shows `Captain (Rust)`.
- grammers never calls `account.updateStatus`, so the daemon should not show Tài as online.
  This is checked live in P0.
- The daemon never calls `messages.readHistory`, `channels.readHistory` or
  `messages.readMentions`. Unread badges stay as they are.
