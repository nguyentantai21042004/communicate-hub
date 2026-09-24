# Agent instructions

Read `docs/CONVENTIONS.md` before changing code and `docs/architecture.md` before changing how
crates depend on each other.

## Before you finish

- Run `make check`. It must pass with no warnings.
- Comments use only the tags in `docs/CONVENTIONS.md`. `scripts/lint-comments.sh` rejects the
  rest.

## Secrets and the live account

- Never print, log, or commit the API hash, an auth key, or a Telethon session string. Do not
  read `~/.config/telegram-captain/env`.
- Never start a second process on a session that is already running, such as the Python demo
  and `captain listen` together. Telegram may revoke the session.
- `captain auth login` is interactive and asks for the owner's phone code and password. The
  owner runs it, not an agent.
- Tests never touch the network or the real Keychain. Use `MemoryStore`. Tests that need an
  account are `#[ignore]`d and gated on `CAPTAIN_LIVE=1`.

## Dependencies

- Do not run a bare `cargo update`. `glass_pumpkin` is held back in `Cargo.lock`; see
  `docs/architecture.md`.
- `refs/` holds read-only reference clones. Read them to check API details. Do not build
  against them or edit them.
