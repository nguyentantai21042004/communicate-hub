# telegram-captain

Capture DMs sent to Tài's personal Telegram account, turn each one into a task, let Claude
save a reply as a Telegram draft, and have Tài review and send it from his own app. Nothing
is sent automatically.

```
teammate DM ──> telegram-mcp (MTProto user session, HTTP :8765, patched)
                  │ capture.json: on/off + threads   <── bin/captain
                  ▼
              incoming_feed.jsonl ──> bin/drafter ──> claude -p (3 tools only)
                                                         │ get_history, save_draft
                                                         ▼
                                    draft in the chat input box, on every device
                                                         ▼
                                    Tài edits, then presses send himself
```

Base: [chigwell/telegram-mcp](https://github.com/chigwell/telegram-mcp) (Telethon, Apache-2.0),
cloned into `candidates/`, plus `patches/0001-capture-switch.patch` (runtime capture switch).

## Setup on the idle MacBook

1. Get an `api_id` and `api_hash` at https://my.telegram.org/apps. The app title must not
   contain "Telegram" (API ToS 2.3). Example: `Captain drafts`.
2. Log in once and keep the session string:

   ```sh
   cd candidates/chigwell__telegram-mcp && uv sync
   TELEGRAM_API_ID=... TELEGRAM_API_HASH=... uv run session_string_generator.py --qr
   ```

   Scan the QR code from the Telegram app on your phone (Settings > Devices > Link desktop
   device). Then write `~/.config/telegram-captain/env` and run `chmod 600` on it:

   ```sh
   TELEGRAM_API_ID=...
   TELEGRAM_API_HASH=...
   TELEGRAM_SESSION_STRING=...
   TELEGRAM_ALLOWED_CHAT_IDS=@hai,@lan      # the agent cannot read or draft outside this list
   ```

3. Start it in the foreground first: `bin/serve` in one terminal, `bin/drafter` in another.
4. Choose what gets captured, live, with no restart:

   ```sh
   bin/captain add @hai      # only these threads become tasks
   bin/captain off           # pause during focus time; the server keeps running
   bin/captain on
   bin/captain status
   bin/captain tasks         # queued / drafted / skipped / failed
   ```

5. When it behaves, run it in the background:

   ```sh
   cp launchd/*.plist ~/Library/LaunchAgents/
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.tantai.captain.serve.plist
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.tantai.captain.drafter.plist
   # stop: launchctl bootout gui/$(id -u)/dev.tantai.captain.serve   (same for .drafter)
   ```

Optional: put what you just shipped (release notes, runbook links) in `prompts/context.md`.
The drafter appends it to Claude's system prompt so drafts can answer ops questions.

## Real-account test checklist

- [ ] `bin/serve` starts and `Settings > Devices` shows "Captain drafts (MacBook 2)".
- [ ] A teammate in the allowlist DMs you: within about 10 s a line appears in
      `~/.local/state/telegram-mcp/incoming_feed.jsonl`, and a draft shows up in that chat
      on your phone.
- [ ] `bin/captain off`, the teammate DMs again: no feed line, no draft.
- [ ] `bin/captain add @hai`, someone else DMs: nothing captured.
- [ ] A DM from someone outside `TELEGRAM_ALLOWED_CHAT_IDS`: nothing captured.
- [ ] Your own outgoing messages and bot messages never create tasks.

## Safety model

- Two independent blocks on sending. The server registers only the read-only tools plus
  `save_draft` (`TELEGRAM_EXPOSED_TOOLS`), so `send_message` does not exist. The drafter's
  `claude -p` also has no built-in tools and an allowlist of three MCP tools.
- One Telegram session, one process. `bin/serve` refuses to start if :8765 is already
  taken, and telegram-mcp locks the session to avoid `AuthKeyDuplicatedError`.
- A broken `capture.json` pauses capture (fails closed) rather than capturing everything.
- The session string is full account access. Keep it in `~/.config`, mode 600, off git.

## Tests

```sh
cd candidates/chigwell__telegram-mcp && uv run pytest -q     # 681 tests, incl. capture switch
# drafter E2E against a fake server (same tool names, prompt-injection bait in history):
candidates/chigwell__telegram-mcp/.venv/bin/python tests/fake_telegram_mcp.py /tmp/calls.jsonl &
XDG_STATE_HOME=$(mktemp -d) sh -c 'mkdir -p $XDG_STATE_HOME/telegram-mcp && echo "{\"chat_id\":123456,\"name\":\"Hải\",\"username\":\"hai\",\"message_count\":2,\"first_message_id\":10,\"last_message_id\":11}" > $XDG_STATE_HOME/telegram-mcp/incoming_feed.jsonl && bin/drafter --once --from-start && bin/captain tasks'
kill %1
```
