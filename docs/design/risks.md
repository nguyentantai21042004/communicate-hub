# Risks

## Telegram API terms

Source: <https://core.telegram.org/api/terms>.

- 1.5: "You are prohibited from using, accessing or aggregating data obtained from the Telegram
  platform to train, fine-tune or otherwise engage in the development, enhancement or deployment
  of artificial intelligence, machine learning models and similar technologies."

  Sending messages to Claude to draft replies reads as "deployment of artificial intelligence"
  with Telegram data. No design choice makes this compliant. Small retention, scope limits and
  a human sending each reply lower the exposure; they do not remove it. The worst realistic
  outcome is a ban of the API id or of Tài's personal account. Tài decides whether to accept
  this before P2 is built. P1 sends nothing to a model.

- 1.4 forbids actions on the user's behalf without knowledge and consent, and tampering with
  read status. Drafts plus manual send, and never calling read methods, cover this.

- 2.3 forbids "Telegram" in the app name. The device model is `Captain (Rust)`.

## Data handling

- Work chats can hold customer data. Each work chat added to scope sends that data
  to Claude in P2. Decide per chat.
- Message text is kept 30 days locally, excluded from Time Machine, never written to metrics or
  logs above `trace`.
- The report prints no message text unless asked with a flag.

## Prompt injection

Anyone in a scoped group can write text that the runner reads. Mitigations:

- The runner has read-only tools plus `save_draft` and `decline`. It cannot send, write to other
  systems, run shell commands or browse.
- `--strict-mcp-config` keeps the machine's other MCP servers, including production ones, out
  of the run.
- Message text reaches the model marked as untrusted data.

What remains: a draft steered by a hostile message. Tài reading the draft before sending is the
last check, and it is why there is no auto-send.

## Session safety

- One process per session, enforced by the lock in [daemon.md](daemon.md).
- The Telethon session string exposed during research must be revoked in Settings > Devices
  once the Rust login works.
