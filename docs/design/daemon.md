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
  stays down. A dropped network at startup uses the same in-process backoff as a dropped
  update stream. Any other start failure, such as a rejected request, exits with an error
  and launchd retries after the throttle interval.
- Logs go to `~/Library/Logs/telegram-captain/<profile>.<date>.log`, one file per day, the last
  14 kept (`captain daemon --log-dir`). stderr goes to `<profile>.stderr.log` and only catches
  what bypasses logging, such as a panic. Logs carry ids, counts and types. Message text only
  at `trace` level.
- Sleep: while the Mac sleeps the daemon is offline. On wake the read fails or the idle
  watchdog fires, the daemon reopens the network in this process, and catch-up asks grammers
  to fill the gap with `updates.getDifference`. Tickets older than the stale threshold go to
  the digest. On a machine meant to stay up, set `pmset -c sleep 0` while on power.

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
| Network drop or ended update stream | Stay in this process. `connection` becomes `reconnecting`. Reopen the network, then catch up. Back off from 1 s to 60 s. See [Reconnect](#reconnect). |
| Idle for 16 min with a failed keepalive | Same reconnect path. A ping that succeeds leaves the in-flight read in place. A ping that reports a dead session stops reading instead. If the home sender is already gone, the ping fails before any request and does not negotiate an auth key. |
| Other RPC error on the open stream | Log it, record it, and retry the same connection with the reconnect backoff (1 s, doubling to 60 s). Non-link failures inside `FAILURE_WINDOW` (60 s) count together, whatever the message text is. At `FAILURE_THRESHOLD` (5), `connection` becomes `degraded` and `degraded_reason` is the class (`other` for an RPC, migrate, transport other than `-404`, I/O, session, invalid datacenter, or deserialize error). A later update, or a restored stream, clears it. |
| `FLOOD_WAIT` or `SLOWMODE_WAIT` on the open stream | Wait the seconds in `(value: N)`, capped at `FLOOD_WAIT_CAP`, and read again on this connection. While that wait is in progress, `connection` is `degraded`, `degraded_reason` is `flood_wait`, and `flood_wait_secs` is the seconds still left. |
| `updates.differenceTooLong` or `updateChannelTooLong` | Some updates are lost for good. grammers resyncs; P1 only sees it as missing messages in recall (M6). P2 records a coverage gap for the affected chats so context tools use the API for that range. |
| `FLOOD_WAIT` during catch-up | grammers 0.10 prints `FLOOD_WAIT (value: N)`. Wait N seconds, capped at `FLOOD_WAIT_CAP` (15 min), then retry catch-up on the same connection. Do not open a new pool. The wait uses the same `flood_wait` status as a flood on the open stream. |
| Auth key revoked | `401`, `SESSION_REVOKED`, `SESSION_EXPIRED`, `AUTH_KEY_*` (including `AUTH_KEY_DUPLICATED`), or transport `-404` (`bad status (negative length -404)`). Stop reading, keep the socket and UI up with status `unauthorized`, and make no further Telegram request. A stored auth key is replaced only by `captain auth login`. Any other `set_dc_option` that brings a different key is refused, latched on the session, and returned as not authorized — not as `session error`. After `AUTH_KEY_NEGOTIATION_CAP` (1) refusal this process does not negotiate another key. Startup then exits 0 with `not_authorized` and tells the owner to run `captain auth login`, instead of `start_failed`. |
| Store write error | P1 logs it at error level for each event and carries on, because nothing acts on a ticket yet. The P2 runner must stop instead: a pipeline that cannot remember what it did must not act. |

## Reconnect

> **Status: written and tested offline, not deployed.** Four independent reviews found that
> grammers negotiates a fresh auth key on a transport `-404` before this code can refuse it,
> so a revoked key can still cause repeated key negotiation. The live daemon keeps the old
> exit-and-restart behavior. Do not run this supervisor on the live account until the open
> findings in the last review (home-datacenter-only latch, one attempt per process, a marker
> file that `captain auth login` clears, exit `Ok` even when `close` fails, no I/O retry
> around the one attempt) are fixed and proven against a real revoked session.

`Connection::next_event` returns `Ok(None)` when grammers reports `InvocationError::Dropped`.
A home-connection read error kills that sender, and the in-flight update wait surfaces as
`Dropped`. The daemon logs show this after `Operation timed out (os error 60)`, and also after
`Network is unreachable (os error 51)` and `Connection reset by peer (os error 54)`. The old
read loop treated `Ok(None)` as fatal, the process exited, and launchd started a new one. If
the network was still down, that new process recorded `start_failed` from `users.getUsers`.

The process, the socket, and the web UI stay up. The supervisor marks `connection` as
`reconnecting`, waits, and calls `Connection::restart_network`. The old sender pool is stopped
before the new one starts, so this auth key is one connection at a time. A handle another
task already holds keeps working, because it reads the client from a shared slot. After the
network is back, catch-up is the startup path: `listen(true)`, then `cache_peers`. One
catch-up runs for each network that comes back.

Backoff is jitter-free. A failed attempt doubles the wait. The wait starts at 1 s again after
the stream has been up for 15 min. The first drop of a process that just started also waits 1 s.

| Constant | Value | Role |
|---|---|---|
| `RECONNECT_INITIAL` | 1 s | First wait, and the wait after `RECONNECT_STABLE` of uptime |
| `RECONNECT_MAX` | 60 s | Cap |
| `RECONNECT_STABLE` | 15 min | Uptime after which the next drop starts at 1 s again |
| `IDLE_TIMEOUT` | 16 min | No update for this long, and a failed keepalive, starts a reconnect |
| `KEEPALIVE_PROBE` | 60 s | How long the ping may take. grammers pings every 60 s (`PING_DELAY`) |
| `RETRY_AFTER` | 5 s | Same-connection retry for a gap, or a non-link error while reopening |
| `FLOOD_WAIT_CAP` | 15 min | Longest sleep for one `FLOOD_WAIT` or `SLOWMODE_WAIT` |
| `FAILURE_WINDOW` | 60 s | Window in which non-link read failures count together |
| `FAILURE_THRESHOLD` | 5 | Failures inside `FAILURE_WINDOW` before `connection` is `degraded` |
| `AUTH_KEY_NEGOTIATION_CAP` | 1 | Refused auth-key replacements before every later call is unauthorized |

`IDLE_TIMEOUT` is 16 min because grammers' `NO_UPDATES_TIMEOUT` is 15 min
(`grammers-session` `message_box::defs`). A quiet chat can sit inside `next_event` for that
long while grammers calls `updates.getDifference` and surfaces no event. The deaths in
`~/Library/Logs/telegram-captain/` are read timeouts (`os error 60`), about 3 to 4 hours of
process uptime apart. The extra minute lets that fetch finish. The watchdog then pings. A ping that succeeds resets the idle
timer and leaves the read pinned. A ping that fails, or takes longer than `KEEPALIVE_PROBE`,
cancels the read and reconnects. A ping only uses a sender that is already open. When that
sender is gone, the ping returns a link error and does not call into grammers, so it cannot
negotiate a new auth key. If the link closes while the ping is in flight, the result is
dropped and any sender that ping created is disconnected. The supervisor then uses the normal
`restart_network` path. A ping that comes back unauthorized does not reconnect: the daemon stays
up with `unauthorized` and issues no further Telegram request. If a probe sees the stored home
auth key change, or the session has refused a replacement, that is unauthorized too.

`SIGINT` and `SIGTERM` also interrupt startup `me`/`listen`, the first `cache_peers`, a
reconnect, catch-up, and the backoff sleep, so the socket and the web server shut down
without waiting for those calls to finish.

A catch-up `FLOOD_WAIT` is not a dead link. The supervisor waits the seconds in
`(value: N)`, at most `FLOOD_WAIT_CAP`, and calls catch-up again on the connection it
already has. `Connection::reopen` runs only after a link failure. An auth failure during
catch-up uses the same stop as a revoked read.

Startup uses the same backoff when `me` or `listen` fails with a timeout, an unreachable
network, a reset, or a dropped sender. Any other startup error still stops the run
(`start_failed` from `me`, `listen_failed` from `listen`). A session that is logged out, or
that refused to replace its stored auth key, still exits 0 with reason `not_authorized`.
A session revoked while the daemon is up stops reading and stays `unauthorized`, with the
socket and the UI still up. The same stop covers `401`, `SESSION_REVOKED`, `SESSION_EXPIRED`,
every `AUTH_KEY_*` name, a transport `-404`, and a refused auth-key replacement.

Each drop is one `stream_drop` row in `metric_events` (no schema change: `kind` is already
text). `at` is when the drop was noticed. `value_a` is the seconds until the stream was back.
`value_b` is 1 when catch-up closed the gap and 0 when it did not. `tag` is `timeout`,
`network_unreachable`, `server_closed`, or `other`. `Ok(None)` and a connection reset are
`server_closed`. The idle watchdog is `timeout`. The class of an error comes from its text
(`os error 60`, `os error 51`, `os error 54`). A database created before this kind still
opens; report queries read the kinds they already know.

`updates.differenceTooLong` stays invisible here. grammers logs it and jumps to the present.
`value_b` is 1 when `listen(true)` and `cache_peers` both succeed, the same assumption as the
startup catch-up tag `ok`. `cache_peers` failing after the stream is up leaves the stream up,
logs the startup warning, and leaves `gap_unfilled` true.

Status JSON adds fields. Existing names stay. `App::status` is what the socket and the web UI
already serve.

| Field | Meaning |
|---|---|
| `connection` | `connected`, `reconnecting`, `degraded`, `unauthorized`, or `offline`. `unauthorized` wins, then `reconnecting`, then `degraded` |
| `degraded_reason` | `flood_wait` while a flood wait is in progress, otherwise the class of the read failures in the window (`timeout`, `network_unreachable`, `server_closed`, or `other`), or null when the stream is not degraded |
| `flood_wait_secs` | Seconds left on an in-progress flood wait, or null |
| `reconnects_24h` | Reconnects that finished inside the last 24 hours. A stamp at time T still counts at T+86400 |
| `last_drop_at` | Unix time of the latest drop, or null when there has not been one |
| `last_drop_secs` | How long that drop lasted, or null while it is still open |
| `gap_unfilled` | True when the latest catch-up did not close the gap |

## What Tài sees on other devices

- The device list shows `Captain (Rust)`.
- grammers never calls `account.updateStatus`, so the daemon should not show Tài as online.
  This is checked live in P0.
- The daemon never calls `messages.readHistory`, `channels.readHistory` or
  `messages.readMentions`. Unread badges stay as they are.
