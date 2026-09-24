# Monitoring UI

## Responsibility

A local web page for watching the inbox during P1: what became a ticket and why, how each
ticket ended, what the metrics say, and whether the daemon is healthy. It also collects the
explicit labels that M5 needs. It sends nothing to Telegram.

## Where it runs

- The daemon serves it on `127.0.0.1:8787`. `CAPTAIN_UI_PORT` changes the port. A taken port
  fails the start instead of moving to the next free one, so two servers cannot pile up.
- `captain ui` prints a signed-in address, and `captain ui --open` opens it in the browser.
- `captain ui serve` shows the same page from the store alone while the daemon is stopped, for
  example to read the report on another copy of the database. It takes the session lock, so it
  cannot run next to the daemon.
- The assets are plain HTML, CSS and JavaScript compiled into the binary. There is no build step
  and nothing is fetched from the network.

## Access

The page shows who wrote to Tài and when, so it is guarded like the socket:

| Check | Stops |
|---|---|
| Bound to the loopback interface only | Other machines on the network |
| `Host` must be `127.0.0.1:<port>` or `localhost:<port>` | DNS rebinding from a web page |
| Token cookie on every API call, from `<state dir>/ui_token` (mode `0600`) | Other local users and other browser profiles |
| `X-Captain` header on every POST | Cross-site form posts; a foreign page cannot set it without a preflight the server never answers |
| `Content-Security-Policy` with `'self'` only, `frame-ancestors 'none'` | Injected scripts, framing |

`captain ui` builds the address as `/?token=<token>`. The server trades the query for an
`HttpOnly`, `SameSite=Strict` cookie and redirects to `/`, so the token leaves the address bar
right away. Deleting `ui_token` signs every browser out; the next `captain ui` makes a new one.

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

## Trying it without an account

```sh
captain --state-dir /tmp/captain-demo dev seed-demo   # 14 days of invented data
captain --state-dir /tmp/captain-demo ui serve        # prints the address; Ctrl-C stops it
```

`seed-demo` refuses a store that already has tickets and needs an explicit `--state-dir`, so it
cannot write into the real profile by accident.
