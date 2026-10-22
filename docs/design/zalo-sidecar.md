# Zalo sidecar

`sidecars/zalo-node` is one Node process per Zalo account. `hub-zalo` spawns it, speaks this contract, and implements `hub_core::NetworkAdapter`. The Rust crate does not import a Zalo protocol library. `zca-js` stays inside the sidecar, pinned at exactly `2.1.2`.

The lists under the markers `contract:methods`, `contract:events`, and `contract:errors` are the names both sides must share. `sidecars/zalo-node/src/contract.ts` is the schema source. Tests fail when those names and the lists below diverge.

## Wire

The process reads stdin and writes stdout. Each frame is one JSON object and a newline, UTF-8, with no other bytes on that stream. Stderr is logs only. A log line is never a frame, and a frame is never written to stderr.

A request is one line on stdin.

```json
{"id": 1, "method": "health", "params": {}}
```

`id` is a non-negative integer. The caller chooses it and must not reuse an id that still has no response. `method` is one of the method names below. `params` is an object. An omitted `params` means `{}`.

A response is one line on stdout and answers exactly one id.

```json
{"id": 1, "ok": true, "result": {}}
```

```json
{"id": 1, "ok": false, "error": {"code": "internal", "message": "internal error"}}
```

`ok` true carries `result` and no `error`. `ok` false carries `error` and no `result`. `error.code` is one of the codes below. `error.message` is a fixed short phrase for that code, with no message text and no credential. `error.retry_after_secs` is present only for `rate_limited`. When Zalo does not say how long to wait, the sidecar sends `30`.

An event is one line on stdout with no `id` and no `ok`.

```json
{"event": "session", "data": {"state": "valid"}}
```

`event` is one of the event names below. `data` matches that event's object.

A line that is not JSON, or that is neither a response nor an event, is ignored by the Rust reader after a warning. The sidecar answers a malformed request with `internal` when it can still see an id, and ignores a line with no id. An unknown method is `unsupported`.

Frames stay at or under 8 MiB. A longer line is discarded through the next newline and reported as `internal` when it was a request.

## Method names

<!-- contract:methods -->

```json
["health","login.qr.start","login.qr.cancel","send.text","send.media","send.reaction","send.unsend","directory.friends","directory.groups","directory.group_members","friend.requests","friend.accept","friend.send_request","group.join","group.leave"]
```

## Event names

<!-- contract:events -->

```json
["message","reaction","undo","group_event","friend_request","session","listener","login.qr.state"]
```

## Error codes

<!-- contract:errors -->

```json
["auth_expired","rate_limited","not_found","unsupported","timeout","internal"]
```

`auth_expired` means the saved session was rejected or there is no session. `rate_limited` means Zalo asked the caller to wait. `not_found` means the user, group, or message is absent. `unsupported` means the method or the payload has no Zalo equivalent. Zalo has no edit API, so an edit is not a method. `timeout` means this process gave up waiting. `internal` is everything else.

## Methods

`health` takes `{}`. It returns `listener` as `up` or `down`, and `session` as `valid`, `expired`, `disconnected`, or `reconnecting`.

`login.qr.start` takes `{}`. It returns `png_base64`, a PNG with the data-URL prefix removed. The same login then emits `login.qr.state` until it finishes or `login.qr.cancel` runs. A second start cancels the one still in progress. The first image is the RPC result. A later image, after the QR expires and the library refreshes it, arrives as `login.qr.state` with `state` `waiting` and a new `png_base64`.

`login.qr.cancel` takes `{}` and returns `{}`. It is safe when no login is running.

`send.text` takes `conversation`, `text`, and optional `reply_to` and `mentions`. `conversation` is a hub id, `zalo:dm:` or `zalo:group:` plus the Zalo thread id. `reply_to` is `{message_id, cli_msg_id?}`. `mentions` is a list of `{uid, pos, len}`. Zalo rejects a text past 2000 characters, so the sidecar splits on a newline or a space when it can, then sends the chunks in order. The result is `message_id` of the last chunk and `message_ids` of every chunk. Empty text is `unsupported`.

`send.media` takes `conversation`, `path`, optional `caption`, and optional `kind`. `path` is a file on this machine. The sidecar uploads that file. A non-empty caption is a follow-up text message, because Zalo has no caption field. The result is `message_id` and `message_ids`, same as text. `kind` `voice` still uses the upload path.

`send.reaction` takes `conversation`, `message_id`, optional `cli_msg_id`, and `emoji`. `emoji` is a Telegram emoji, or null to clear. The sidecar maps the emoji to a Zalo icon. An emoji with no Zalo icon is `unsupported`. Null sends Zalo's empty icon, which clears the reaction.

`send.unsend` takes `conversation`, `message_id`, and optional `cli_msg_id`. It returns `{}`.

`directory.friends` takes `{}` and returns `friends`, a list of `{id, name}`. A local alias wins over the Zalo display name.

`directory.groups` takes `{}` and returns `groups`, a list of `{id, name}`.

`directory.group_members` takes `group_id` and returns `members`, a list of `{id, name}`. Names are cached for 5 minutes. The cache prefers `currentMems` on the group info. Missing names are filled from the PC app profile API when `app-session.json` sits next to the credentials file and is mode `0600`. Otherwise the web `getGroupInfo` and `getUserInfo` calls are the fallback. The PC app login flow that creates `app-session.json` is not a method here.

`friend.requests` takes `{}` and returns `requests`, a list of `{id, name, message?}`. The web recommendations endpoint is the list Zalo's own app reads.

`friend.accept` takes `user_id` and returns `{}`.

`friend.send_request` takes `user_id` or `phone`, plus `message`. A phone is resolved with the user search first. A missing user is `not_found`. It returns `{}`.

`group.join` takes `link` and returns `{}`.

`group.leave` takes `group_id` and returns `{}`.

## Events

`message` data:

| Field | Meaning |
|---|---|
| `conversation` | Hub id `zalo:dm:` or `zalo:group:` |
| `message_id` | Zalo message id, raw, with no `zalo:` prefix |
| `cli_msg_id` | Present when Zalo sent one other than `0` |
| `sender_id` | Raw user id |
| `sender_display_name` | Name Zalo put on the message, or empty |
| `text` | Plain text. Empty when the message is attachments only |
| `attachments` | List of attachment objects, in display order |
| `reply_to` | `{message_id}` when the message quotes another |
| `sent_at_unix_secs` | Unix seconds |
| `mentions` | `{user_id, display_name}` |
| `from_self` | True when the logged-in account sent it |

An attachment has `kind`, optional `mime`, `size_bytes`, `local_path`, `url`, and `file_name`. `kind` is `photo`, `video`, `voice`, `audio`, `file`, `sticker`, `gif`, `location`, `poll`, `bank_card`, or `contact`. Downloaded bytes use `local_path`. A location uses `url` and no file. A contact with no QR image uses `url`.

The sidecar drops a delete notice, a blank text, and a media message with no bytes and no stand-in. It keeps a poll, a contact card, an e-card, and a location even when the body is empty. An e-card is plain text plus a photo attachment when the card has an image. There is no `ecard` kind on the wire, because `hub-core` has no e-card attachment. A link is plain text that contains the URL. A missed call is the text `cuộc gọi thoại nhỡ` or `cuộc gọi video nhỡ`.

A bank card's text is the bank name, the account number, and the holder, one per line. The QR PNG is the `bank_card` attachment. A poll's question and options are the message text, and the attachment kind is `poll`. A sticker the library can only describe as a sprite sheet is sent as a photo, with the text `(sticker động)` when the body was empty. A sticker lookup that fails becomes the text `[sticker]`.

`reaction` data is `conversation`, `message_id`, `emoji`, and `added`. `emoji` is one of the Telegram bot reaction emoji. An icon outside that set is not emitted. An empty Zalo icon is a removal: `added` false and `emoji` empty.

`undo` data is `conversation` and `message_id`.

`group_event` data is `conversation`, `text`, `sent_at_unix_secs`, and `kind`. `kind` is `join`, `leave`, `remove_member`, `block_member`, `join_request`, `rename`, or `other`. `text` is one English sentence the bridge can mirror. A rename's sentence contains the new name.

`friend_request` data is `sender_id`, `sender_display_name`, and `message`. `message` is empty when the request had no greeting. The sidecar emits this only for a request someone else sent.

`session` data is `state` and optional `detail`. `state` is `valid`, `expired`, `disconnected`, or `reconnecting`. `detail` is a short token such as `not_logged_in` or `close_1006`, never message text.

`listener` data is `state` `up` or `down`, and `reason`, a short token.

`login.qr.state` data is `state` `waiting`, `scanned`, `expired`, or `done`. `scanned` may include `display_name`. `waiting` may include `png_base64`. A phone that declines the QR is reported as `expired`, and the login stops.

## Media files

The sidecar downloads Zalo file bytes into `<data-dir>/media` and puts that path in `local_path`. It does not delete them. `hub-zalo` deletes a file in that directory whose mtime is older than `MEDIA_RETENTION`, which is 7 days. The cleanup function only unlinks regular files in that one directory.

A location is not a file. Its `url` is `https://maps.google.com/maps?q=<lat>,<lng>`. A contact with no image uses `https://zalo.me/u/<uid>`.

## Process

The environment variable `ZALO_CREDENTIALS_PATH` is the credentials file. The sidecar reads it only after the mode is `0600`. If the mode is wider and the process owns the file, it changes the mode to `0600` before reading. If the mode stays wider, it refuses the file. The file contents, the cookie, the imei, and the user agent are never written to a log. A new login writes the same three fields and then sets the mode to `0600`.

`ZALO_DATA_DIR` is the data directory. When it is unset, the directory of the credentials file is the data directory. The listener lock is `<data-dir>/listener.lock`. The file contains the pid. Startup creates it exclusively. If the file exists and that pid is alive, the process writes `refusing to start: listener lock is held` to stderr and exits 1. A dead pid is replaced. Exit and `SIGTERM` remove the lock. A second listener for the same data directory does not start.

Stdout stays quiet until the lock is held. The sidecar then logs in with the saved credentials when the file exists. Failure to log in emits `session` `expired` and leaves the process up so a QR login can repair it.

The listener reconnects after an unintended close. Close code `1000` and an explicit stop do not reconnect. The first delay is 5 seconds, then double, and the cap is 5 minutes. An auth failure during reconnect emits `session` `expired` and the delay schedule continues. A successful reconnect resets the delay to 5 seconds and emits `session` `valid` and `listener` `up`.

One account is one process. The Rust supervisor is the one that starts it.

## Rust supervisor

`hub-zalo` starts the process with a program and an argument list. It does not use a shell. It writes requests and reads stdout on one task, correlates responses by `id`, and applies `request_timeout`. A response after the timeout is ignored. Stderr lines are warnings and are not parsed.

An unexpected exit fails the calls that were in flight, then waits with exponential backoff before a new process. The initial delay, the cap, and the crash-loop limit are named constants. The default initial delay is 1 second, the delay cap is 5 minutes, and the crash-loop cap is 5 unexpected exits in a row. A process that produces a frame resets the streak. After the cap, calls fail and health is `down`. A requested shutdown closes stdin, does not restart, and `next_event` returns no more events.

`ModuleHealth.module` is `zalo-bridge`. `detail` is `listener=<up|down> session=<state> last_restart=<unix> restarts_24h=<n>`, plus ` crash_loop=1` when the cap has been hit. `last_restart` is `0` when the process has never been restarted. The initial spawn is not a restart. `state` is `ok` when the listener is up and the session is `valid`, `down` when the session is `expired` or the crash loop has tripped, `degraded` while reconnecting or disconnected, and `unknown` before the first spawn.

`session` `valid` becomes `SessionChanged` `valid`. `expired` becomes `expired`. `reconnecting` becomes `connecting`. `disconnected` updates health only. `hub_core::SessionState` has no disconnected value, so the adapter does not invent `logged_out` or `expired` for a dropped socket.

`listener` updates health only. It is not a `RemoteEvent`.

`login.qr.state` is not a `RemoteEvent`. `ZaloAdmin` reads it for the dashboard.

Messages, reactions, undos, group events, and friend requests become the matching `RemoteEvent` values. A message the sidecar just sent is not echoed back.

## Behaviour kept from the existing bridges

Text, reply quotes, mentions, photo including the HD url, doodle as photo, voice, video, gif, file, sticker, location, poll, contact, link, and e-card follow the chat-bridges normalizer. Reaction icons use that project's map onto the Telegram bot emoji set, both directions. Group member names follow the PC app cache in `zalo-bridge-telegram`, then the web API. Bank card HTML parsing and the e-card title, description, and notify line follow that same project.

Albums are not merged. Each photo is its own message. Rich-text style spans are parsed and then dropped, because hub text is plain. Poll vote scores are not edited in place. A poll arrives as one message. The PC app QR login that writes `app-session.json` is not started by this sidecar.
