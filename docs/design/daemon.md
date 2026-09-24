# Daemon and IPC

## Responsibility

`captain daemon` is the only process that opens the Telegram session. It receives updates,
runs the inbox pipeline, keeps the store, serves the socket, and in P2 runs Claude. Every other
command is a client.

## Why one process

Two processes using one auth key make the server see two clients with the same key and
diverging update state. In research this got a Telethon session revoked. The current CLI opens
its own `Connection` per command, so `captain send` while `captain listen` runs is already
unsafe. The daemon makes the safe path the only path.

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
| Store writer | SQLite connection | A dedicated thread; other tasks send it commands. SQLite writes are blocking. |
| IPC server | Unix socket | One task per client connection |
| Timers | debounce deadlines, ticket resolver, retention, metrics snapshots | Driven by one scheduler task |
| Runner (P2) | `claude -p` child processes | Queue of one |

Each task has an owner that awaits or cancels it on shutdown, as `docs/CONVENTIONS.md` requires.
Shutdown order: stop IPC, stop the runner, drain the pipeline, `Connection::close` (flushes
session state), close the store.

## IPC

- Socket: `<state dir>/captain.sock`, mode `0600`, directory `0700`.
- The server checks the peer uid with `getpeereid` and rejects any uid other than its own.
- Protocol: JSON lines. One request object, one response object, same `id`. `subscribe` keeps
  the connection open and streams events.
- No TCP port. If the runner ever moves to another machine, that becomes an SSH-forwarded
  socket, decided then.

| Method | Caller | Purpose |
|---|---|---|
| `status` | CLI | Connection state, paused flag, queue length, uptime |
| `pause`, `resume` | CLI | Global switch for ticket creation |
| `subscribe` | `captain listen` | Stream of events, as `listen` prints today |
| `send`, `download` | CLI | Existing commands, now through the daemon |
| `tickets.list`, `tickets.show`, `tickets.label`, `ticket.new` | CLI | Inspect, label, create manually |
| `scope.reload` | CLI | Reread `scope.toml` |
| `report` | CLI | Aggregates for `captain report` |
| `context.*`, `draft.save`, `ticket.decline` | `captain mcp` (P2) | Tools in [context-tools.md](context-tools.md) |

`captain mcp` is a stdio MCP server that the runner starts. It holds no session; it forwards
tool calls to the daemon over this socket, tagged with the ticket id so the daemon can apply
that ticket's budget.

## Running on the second Mac

- launchd agent `dev.tantai.telegram-captain.plist` in `~/Library/LaunchAgents`, with
  `RunAtLoad`, `KeepAlive` and a `ThrottleInterval` so a crash loop does not hammer Telegram.
  A LaunchAgent, not a LaunchDaemon, because the Keychain is per user and unlocked at login.
- stdout and stderr go to `~/Library/Logs/telegram-captain/`. Logs carry ids, counts and
  types. Message text only at `trace` level.
- Sleep: set `pmset -c sleep 0` on that machine while on power. The daemon still handles wake:
  grammers catches up with `updates.getDifference`, and tickets older than the stale threshold
  go to the digest instead of producing a late draft.

## Failure handling

| Failure | Behaviour |
|---|---|
| Network drop | grammers reconnects. The daemon records the gap for the uptime metric and the store's coverage. |
| `updates.differenceTooLong` or `updateChannelTooLong` | Some updates are lost for good. Record a coverage gap for the affected chats; context tools use the API for that range. |
| `FLOOD_WAIT_X` | Wait X seconds on that method only, record it, never retry sooner. |
| Auth key revoked (401) | Stop the pipeline, keep the socket up with status `unauthorized`, and exit with a distinct code only when asked. Do not let launchd restart into a login loop. |
| Store write error | Fail loudly and stop creating tickets. A pipeline that cannot remember what it did must not act. |

## What Tài sees on other devices

- The device list shows `Captain (Rust)`.
- grammers never calls `account.updateStatus`, so the daemon should not show Tài as online.
  This is checked live in P0.
- The daemon never calls `messages.readHistory`, `channels.readHistory` or
  `messages.readMentions`. Unread badges stay as they are.
