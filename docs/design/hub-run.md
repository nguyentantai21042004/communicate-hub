# Hub process

`captain hub run` is a second process beside `captain daemon`. The daemon holds the owner's
Telegram user session and `session.lock`. The hub holds `hub.lock`, the Zalo sidecar, the bot
adapter, and the bridge. They share `inbox.sqlite` and nothing else. A Zalo crash does not
take the inbox down, and the hub never opens the owner's MTProto session.

The bot token is a Keychain item. The service is `dev.tantai.comms-hub` and the account is
`bot-token`, stored as `{profile}/bot-token`. It is not the MTProto service, it is not an
environment variable, and it is not a field in `hub.toml`.

## What run starts

1. Take `hub.lock` in the profile state directory. A second hub exits without waiting.
2. Read `hub.toml`.
3. Open `inbox.sqlite` through `SharedHub` and store the bridge group id.
4. Create `zalo/credentials.json` with `{}` and mode 0600 when the file is missing. An existing
   file is left untouched and is not read.
5. Start the sidecar. The default program is `node` and the default argument is
   `sidecars/zalo-node/dist/main.js`, found by walking up from the current directory and from
   the binary. Set `sidecar_args` when that file is not there.
6. Start the bridge on a `SqliteStore`. Incoming Zalo events pass through a gate that drops a
   message whose network id is already mapped, and drops a muted conversation when
   `skip_muted_zalo_groups` is true.
7. Long-poll Telegram on the bot's own HTTP client, outside the five send slots.
8. Write `module_health` for `zalo-bridge` and `bridge` at start, every 15 seconds, and when
   the snapshot changes. Write `module_events` for a listener drop, a sidecar restart, a QR
   relogin, a bridge rate limit, and a new unresolved failure.
9. Prune events and expired Zalo media once an hour. The first interval tick is discarded.
10. On SIGINT or SIGTERM, stop the poll, drain the dispatch queue, stop the bridge, shut the
    sidecar down, and write both modules as `down`.

`/status`, `/checktopics`, and `/topic list` or `/topic info` answer anyone in the group.
`/login` and `/recall` are admin-only. `/login` sends the QR image into the topic. `/recall`
needs a reply. Any other slash command gets `command not available` and is not forwarded.
Muted groups are skipped only when the flag is set and the conversation row is already muted.
The sidecar does not send a muted field.

## Files

| Path | Who writes it |
|---|---|
| `<state>/hub.toml` | The owner. The token must not be in it. |
| `<state>/hub.lock` | `hub run`, `zalo login`, and `zalo logout`. |
| `<state>/inbox.sqlite` | The hub and the Captain daemon. |
| `<state>/zalo/credentials.json` | The sidecar. `zalo logout` deletes it without reading it. |
| `<state>/zalo-qr.png` | QR login, mode 0600. |
| `~/Library/Logs/comms-hub` | launchd and `hub run --log-dir`. |

`hub.toml` fields: `group_id` (negative), optional `ops_topic_id` (positive, fits in i32),
`admin_user_ids`, `skip_muted_zalo_groups` (default false), `avatar_card` (default true),
`backfill_rename_cap` (default 20), `backfill_rename_gap_secs` (default 3, zero allowed),
`sidecar_program`, `sidecar_args`, `event_retention_days` (default 30),
`media_retention_days` (default 7). A bad file errors with `hub.toml:` and the field name.
A `bot_token` key is rejected and its value is not copied into the error.

## Owner steps

Do these yourself. Nothing in this tree starts the hub, the sidecar, or launchd.

1. `captain hub auth set-bot-token`. A terminal prompts with hidden input. A pipe reads one
   line from stdin. The value goes to the hub Keychain entry and is not echoed.
2. Write `<state>/hub.toml` with the forum group id, the admin user ids, and the sidecar
   command if the repo script is not on the walk-up path.
3. `captain hub preflight`. This calls the Bot API and prints the forum verdict.
4. `captain hub zalo login`. The QR is drawn in the terminal and written to `zalo-qr.png`.
   Ctrl-C cancels. This takes `hub.lock`, so stop the hub first or use `/login` in the group
   while the hub is running.
5. `scripts/install-hub-agent.sh`. It writes the LaunchAgent and does not call `launchctl`.
   Start it with the printed `launchctl bootstrap` command when the checks above passed.
   Stop it with the printed `bootout` command.
6. `captain ui serve --attach` serves the new dashboard on port 8788 from `inbox.sqlite` while
   the old daemon keeps the session and its UI on 8787.

`captain hub status` reads `module_health` when `inbox.sqlite` already exists. It does not
create the file. `captain hub zalo logout` deletes `zalo/credentials.json` and does not call
Zalo.

## LaunchAgent

`scripts/install-hub-agent.sh` writes `~/Library/LaunchAgents/dev.tantai.comms-hub.bridge.plist`.
The label is `dev.tantai.comms-hub.bridge`, one agent, not one per profile. The profile is an
argument: `captain --profile <name> hub run --log-dir ~/Library/Logs/comms-hub`. `KeepAlive` is
true and `ThrottleInterval` is 30 seconds. `--dry-run` prints the plist and the `launchctl`
commands, lints the plist, and writes nothing. The install writes the plist and does not
call `launchctl`.

## What this process does not do

`hub-core` has no outgoing mentions, no `reply_to` on media, and no `disconnected` session
state. Those are dropped or kept as health text. Topic rename is `TopicSink::rename_topic`;
a sink that does not override it returns `Unsupported` and the bridge skips that call.
`hub-zalo` emits
`from_self` with no field the bridge can see, so only an already-mapped network id is dropped.
Voice duration, rich-text style, poll score edits, e-card or link cards, and Zalo-side album
merge are not available on the types this process is allowed to use. A Telegram file id cannot
sit on a `hub-core` attachment, so topic media becomes a `[files: ...]` note instead of a Zalo
upload. `dashboard::Adapter::modules` does not read this store. The commands outside the set
above are out of scope.

## Cost

Friends and groups are fetched once when the sidecar listener starts, then at most once per
24 hours (`DIRECTORY_TTL_MS`), plus at most one more fetch in that window when an event names
an unknown conversation. `rate_limited` backs off and is not retried in a loop. One directory
fetch is in flight at a time. Group member lists load only when a sender or mention has no
name, stay cached for 5 minutes, and keep at most 64 groups (`GROUP_MEMBER_CACHE_MAX`).

A new conversation costs one `createForumTopic`. `getForumTopicIconStickers` runs once per
process and is reused. When the directory list already included an avatar URL, one `sendPhoto`
and one `pinChatMessage` run once per conversation (`avatar_card`, default true). A later
title change is one `editForumTopic`, at most once per conversation per 24 hours
(`RENAME_INTERVAL_SECS`). One backfill pass runs per directory refresh, at most 20 renames
(`BACKFILL_RENAME_CAP`), with 3 seconds between them (`BACKFILL_RENAME_GAP_SECS`). The wait
ends with the pass. There is no new timer. The idle loop is still the 30 second state tick,
the 60 second health tick, and the hourly prune.
