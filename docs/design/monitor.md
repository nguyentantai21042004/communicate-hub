# Monitoring UI

## Responsibility

A local web page for watching the inbox during P1: what became a ticket and why, how each
ticket ended, what the metrics say, and whether the daemon is healthy. It also collects the
explicit labels that M5 needs. It sends nothing to Telegram.

## Where it runs

- The daemon serves it on `127.0.0.1:8787`. `CAPTAIN_UI_PORT` changes the port.
  `CAPTAIN_UI_HOST=hub.localhost` prints sign-in URLs with that name (default `127.0.0.1`).
  A taken port fails the start instead of moving to the next free one, so two servers cannot
  pile up.
- `captain ui` prints a signed-in address, and `captain ui --open` opens it in the browser.
- `captain ui serve` shows the same page from the store alone while the daemon is stopped, for
  example to read the report on another copy of the database. It takes the session lock, so it
  cannot run next to the daemon.
- `captain ui serve --attach` serves the same pages from the shared `inbox.sqlite` next to a
  running daemon and `captain hub run`. It does not take `session.lock` or `hub.lock`, does not
  open Telegram, and does not create or migrate the store. Default port is 8788 (`CAPTAIN_UI_PORT`
  still overrides; a taken port still fails). The token is the existing `ui_token` file; it is
  not rotated. The printed output includes both `127.0.0.1` and `hub.localhost` sign-in URLs.
  The daemon's own UI on 8787 is unchanged.
- The assets are plain HTML, CSS and JavaScript compiled into the binary. There is no build step
  and nothing is fetched from the network.

## Access

The page shows who wrote to Tài and when, so it is guarded like the socket:

| Check | Stops |
|---|---|
| Bound to the loopback interface only | Other machines on the network |
| `Host` must be `127.0.0.1:<port>`, `localhost:<port>`, or `hub.localhost:<port>` | DNS rebinding from a web page |
| Token cookie on every API call, from `<state dir>/ui_token` (mode `0600`) | Other local users and other browser profiles |
| `X-Captain` header on every POST | Cross-site form posts; a foreign page cannot set it without a preflight the server never answers |
| `Content-Security-Policy` with `'self'` only, `frame-ancestors 'none'` | Injected scripts, framing |

`captain ui` builds the address as `/?token=<token>`. The server trades the query for an
`HttpOnly`, `SameSite=Strict` cookie (host-only, no `Domain`) and redirects to `/`, so the
token leaves the address bar right away. `hub.localhost` and `127.0.0.1` are separate cookie
jars; each sign-in URL sets its own. Deleting `ui_token` signs every browser out; the next
`captain ui` makes a new one.

Another name such as `hub.tantai.dev` would need an `/etc/hosts` entry (sudo, the owner does
it) plus adding the name to the const list; exposing the UI beyond loopback is out of scope.

A rejected `Host`, and any POST without `X-Captain`, answers 403. An `/api` call without the
cookie answers 401 with `{"error": "..."}`.

Message text is never part of a list response. The ticket detail loads text only when Tài
presses Show text, and the page inserts it as text, never as HTML.

## Screens

| Screen | Shows | Reads |
|---|---|---|
| Status bar, every screen | Connection, pause switch, last update age, open bursts, last day's counts | `GET /api/status` |
| Tickets | Filterable list; detail with trigger, route, timeline, outcome, and the want, noise or skip label | `GET /api/tickets`, `GET /api/tickets/{id}`, `POST /api/tickets/{id}/label` |
| Metrics | Decision rules, tickets per day by route, Q1 to Q6 with breakdowns | `GET /api/report?days=` |
| Scope | Watched chats, out-of-scope candidates with a copyable `[[chat]]` entry, policy values | `GET /api/scope`, `POST /api/scope/reload` |
| Health | Live state and Q7 (uptime, gaps, update lag, API pressure, store growth) | `GET /api/status`, `GET /api/report` |

Pause and resume are `POST /api/pause` and `POST /api/resume`. Every API route answers the same
JSON as the matching socket method, because both call the same functions in `app.rs`.

`--attach` opens a read-only `SQLite` connection (`query_only`, busy timeout, no migrations).
`GET /api/status` includes `read_only: true`. If the daemon's socket answers, that status is
shown as-is; otherwise the connection card is `offline` ("Offline view") from the store.
Write routes answer 403 `{"error": "read-only attach mode"}`: `POST /api/pause`,
`POST /api/resume`, `POST /api/tickets/{id}/label`, `POST /api/scope/reload`,
`POST /api/history`, `POST /api/zalo/relogin`, and `POST /api/zalo/retry/{id}`. The pages hide
or disable those controls. The process has no background jobs beyond the HTTP server. A missing
store file or a newer schema than this binary knows exits non-zero.

## Dashboard endpoints

The later pages read the routes below. They sit on the same router, so the loopback bind,
`Host` check, cookie, `X-Captain` header, and content security policy apply to them too.
Existing routes keep their JSON byte for byte.

| Method and path | Query or body | Returns |
|---|---|---|
| `GET /api/overview` | `days` (default 14, at least 1, clamped to 180), `source` (`all`, `telegram`, or `zalo`) | KPI, daily series, up to 6 needs-attention rows, module strip |
| `GET /api/conversations` | `source`, `kind` (`dm`, `group`, `channel`, `forum`), `days`, `q`, `limit` (default 50, clamped to 200), `offset` (clamped to 10000) | Rows and `total` |
| `GET /api/analytics` | `days`, `source` | 7 by 24 heatmap, ticket funnel, top conversations, response p50 per day |
| `GET /api/telegram` | `days` | Connection, session, four triggers, scope funnel, out-of-scope candidates, top senders |
| `POST /api/history` | `{chat, regex, scan_limit, text}` | Daemon history matches. `text` defaults to false. `scan_limit` defaults to 3000 and clamps to 20000 |
| `GET /api/zalo` | `days` | Session, send queue, topics, message kinds, failed events, friend requests, groups without a topic |
| `GET /api/modules` and `GET /api/healthz` | none | `ModuleHealth` for `captain-daemon`, `zalo-bridge`, and `bridge` |
| `GET /api/events` | `limit` (default 50, clamped to 200) | Module events, newest first |
| `POST /api/zalo/relogin` | none | Starts the QR flow, or 501 until a Zalo admin exists |
| `POST /api/zalo/retry/{id}` | none | Asks the bridge to retry that failed event, or 501 until it can |

A bad query or body answers 400 with `{"error": "..."}`. Unknown query keys are ignored.
`days` above 180 is clamped. `days` below 1 is rejected. An empty `regex` is rejected.

`messages_24h` counts both directions over the last 24 UTC hour buckets, including the hour
of the request. It does not reset at 00:00 UTC. The daily series is one point per local day
in the window, oldest first, using the machine's UTC offset. A selected source with no
messages is a zero. Sparkline buckets on the conversation list stay on UTC days. The
`messages_24h` and `DailyMessages.day` comments in `hub-web` still say UTC. The handler
follows the rolling window and the local offset instead.

Needs attention is tickets still queued, oldest first, at most 6 rows. The KPI count is the
full queued set for the selected source. Digest tickets are stored and counted inside
hub-analytics, and the funnel JSON has no digest field. The analytics funnel counts tickets
opened inside the window. `queued` is a count, `draft` is the string `not available in P1`,
and `resolved` is grouped by label.

The heatmap is weekday by local hour, Monday 0 through Sunday 6. Hour buckets are shifted by
the machine's UTC offset. All 168 cells are returned. `top_senders` is the display name and
the count of stored incoming messages in the window. Message text is not returned. `events`
lists `module_events`, newest first. `low_sample` is true when a day's reply sample is below 5.
hub-analytics still buckets the daily reply median by UTC day of the anchor. Active chats are
unmuted conversations with activity in the requested range, which is wider than the current
UTC day described on `ActiveWatched`.

The Telegram session card names the account and the storage kind (`keychain`). It does not
return the session secret. `reconnects_24h` is the live daemon counter.
`last_catch_up_unix_secs` is the start of the latest run in the requested window that caught
up. `uptime` and `gaps_unfilled` use that same window.

`POST /api/history` calls the daemon search. A match includes `link` only when Telegram has a
`t.me` URL for it. `text` is included only when the body sets `text` to true. List endpoints
do not include message text.

Zalo user ids and chat ids are masked to the first four and last four characters. Telegram
ids stay exact on the conversation list and inside the `[[chat]]` snippet a page copies.
That snippet is Telegram only.

When the Zalo bridge has no heartbeat row, `GET /api/zalo` still answers 200. `bridge.state`
is `down` and `bridge.detail` is `no heartbeat`. Numeric cards are the last stored numbers,
and they are zero when the tables are empty. `rate_limits_24h` counts `rate_limited` module
events in the last day. There is no send-queue table, so `waiting` stays 0.
`pending_friend_requests` counts `friend_request` module events. The Zalo session account is
empty until a session table exists. `POST /api/zalo/relogin` and `POST /api/zalo/retry/{id}`
answer 501 with `{"error": "..."}` until a Zalo admin can run them.

## Trying it without an account

```sh
captain --state-dir /tmp/captain-demo dev seed-demo   # 14 days of invented data
captain --state-dir /tmp/captain-demo ui serve        # prints the address; Ctrl-C stops it
captain --state-dir /tmp/captain-demo ui serve --attach  # port 8788; store must already exist
```

`seed-demo` refuses a store that already has tickets and needs an explicit `--state-dir`, so it
cannot write into the real profile by accident. It fills the captain inbox and the hub tables
in that directory's `inbox.sqlite`, and writes `scope.toml` when that file is missing.

The seeded window ends at the moment of the command. The pages should show:

- 119 Telegram chats watched, and 41 Zalo conversations (16 DMs and 25 groups, 2 of those groups muted, so the Zalo watched count is 39).
- 412 draft-route tickets opened in those 14 days: 6 still queued and 406 resolved (Answered 187, Answered elsewhere 44, Read only 96, Unseen 79). Three more tickets sit in the digest. The funnel JSON does not include that digest count.
- A response median of 18 minutes over 187 replies.
- 1,012 Telegram messages and 272 Zalo messages in the rolling 24 hours.
- One 429 on the Zalo send path in that day, 3 pending friend requests, 7 Zalo groups without a topic, and 4 failed deliveries.
- Module events for a stream drop and reconnect, a Zalo listener drop and reconnect, and a Zalo QR login.

Names are invented, for example "Nhóm Kho Quận 7", "Anh Hùng (DM)", "Release room", and "Backend team". Message text is stored only so the Telegram sender list can be checked. The dashboard routes do not return it.

## Pages

The tab bar order is Tổng quan, Hội thoại, Phân tích, Telegram, Zalo, then the existing
Tickets, Metrics, Scope, and Health tabs. The four English tabs keep their labels. An empty
hash opens Tổng quan.

Tổng quan and Phân tích share a source switch and a day window. The page stores that pair in
`localStorage` under `captain.range`. Hội thoại stores its own filters under
`captain.conversations`. Telegram and Zalo store only their day window, under
`captain.telegram` and `captain.zalo`.

Pause asks before it stops new tickets. Resume does not ask. Đăng nhập lại Zalo (QR) asks,
then calls `POST /api/zalo/relogin`. A 501 shows "Chưa kết nối bridge." The page draws a QR
only when that response includes a same-origin image or a `data:image` payload.

Each new page has a loading skeleton, an empty state, and an error state. Zalo and Health
show a bridge-down banner when the bridge state is `down`. Charts are inline SVG with a text
summary and a tooltip on hover and on focus. Message text is inserted with `textContent`, and
only after Show text on the history search.

`apps/captain-cli/web/format.mjs` formats counts, ages, heatmap levels, and sparkline heights.
`make web-test` runs its tests. The page scripts are separate files under `web/`, linked from
`index.html`, and served from `/assets/` by `web.rs`.
