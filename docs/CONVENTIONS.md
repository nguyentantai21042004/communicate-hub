# Code conventions

These rules apply to every crate in this workspace. `make check` enforces the ones a tool can
check (formatting, lints, comment tags). Review enforces the rest.

## Layers and dependency direction

```
captain-cli ──┬──> captain-format ────> captain-core
              ├──> captain-protocol ──┬─> captain-core
              │                       └─> captain-vault
              └──> captain-vault
```

- `captain-core` holds protocol-agnostic types and Telegram limits. It does no I/O and depends on
  no Telegram library.
- `captain-protocol` is layer 1: it converts between Telegram (MTProto via grammers) and core
  types. It never parses Markdown or HTML.
- `captain-format` is layer 2: it turns authoring formats into `FormattedText`. It never talks to
  the network.
- `captain-vault` stores secrets and local state paths. It knows nothing about MTProto.
- Only `apps/` may depend on everything. Library crates never depend on an app.

## Naming

- Crates: `captain-<layer>`. Modules: short nouns (`inbound`, `outbound`, `session`).
- Types are nouns, functions are verbs. Conversions follow std: `from_*`, `to_*`, `into_*`, `as_*`.
- A unit belongs in the name: `timeout_secs`, `max_bytes`, `offset_utf16`. A bare `timeout` or
  `size` is not allowed when the unit is not obvious from the type.
- Each library crate exposes one `Error` enum and `pub type Result<T> = std::result::Result<T, Error>`.

## No magic values

A literal that carries meaning becomes a named `const`. This covers limits, timeouts, sizes, file
names, Keychain service names, and protocol strings.

- Put Telegram-imposed limits in `captain_core::limits`, and cite where each number comes from.
- Put a group's notes as trailing comments on the same line, not as lines above each member:

  ```rust
  pub const CAPTION_MAX_UTF16: usize = 1024; // non-Premium media caption
  pub const ALBUM_MAX_ITEMS: usize = 10;      // messages.sendMultiMedia hard cap
  ```

- A module that is only a table of related constants (like `limits`) starts with
  `#![allow(missing_docs)]`. Its `//!` doc says where the numbers come from, and each member
  keeps its trailing comment. This is the one sanctioned exception to `missing_docs`.
- Literals allowed inline: `0`, `1`, empty strings, format strings, and test data.

## Errors and panics

- Libraries use `thiserror`. `anyhow` is allowed only in `apps/`.
- Library code does not `unwrap()` (denied by clippy). An `expect()` needs an `INVARIANT:` comment
  directly above it, saying why the value cannot be absent.
- Error messages are lowercase, have no trailing period, and name the thing that failed:
  `"keychain entry {name} is not valid UTF-8"`.
- Map third-party errors at the crate boundary. A caller of `captain-protocol` never has to
  match a grammers type.

## Comments

The policy is adapted from the global Go comment policy. Rustdoc replaces godoc.

### Doc comments (`///`, `//!`)

- Every public item has a doc comment (`missing_docs` warns). The first sentence is a one-line
  summary of the caller-facing contract, not of the implementation.
- Keep it to one to three sentences by default. Mention inputs, outputs, side effects, and edge
  cases only when callers need them.
- A public function that returns `Result` documents its failure cases under `# Errors` when a
  caller can act on them. Add `# Panics` only if it can panic.
- Each crate root and each module starts with `//!`: what it owns, and what it must not do.
- Traits carry the contract on the trait method. Implementations do not repeat it. This differs
  from the Go rule on purpose: in Rust, callers read the trait, and rustdoc shows trait docs on
  every implementation.
- Trivial builder setters get one line that says what the option does in Telegram terms, for
  example "Sends without a notification sound on the recipient's side."
- Private functions get a doc comment only when the behaviour is not obvious, has external side
  effects, coordinates concurrency, or encodes a business rule that is easy to break.

### Inline comments (`//`)

- Explain why, not what. A comment that restates the next line gets deleted.
- Put the comment directly above the code it explains, or at the end of the line for short notes.
- When behaviour changes, update or delete the comment in the same commit.

### Comment tags

A tag goes at the start of a comment and is followed by a colon. Only the tags below are allowed.
`scripts/lint-comments.sh` rejects anything else (`NOTE`, `HACK`, `XXX`, `IMPORTANT`, bare
`TODO`).

| Tag | Use for | Required content |
|---|---|---|
| `TODO(owner):` | Planned work that is not done yet | What is missing and why it waits |
| `FIXME(owner):` | A known defect that ships anyway | The failure and its impact |
| `INVARIANT:` | An assumption the code depends on | Why it holds. Required above every `expect()` |
| `TELEGRAM:` | Server behaviour or a protocol quirk | The TL constructor, layer, or doc URL |
| `SECURITY:` | A decision about secrets or trust | What it protects against |
| `PERF:` | A deliberate performance trade-off | What was measured or assumed |

`owner` is a person's handle, for example `TODO(tantai): ...`.

## Secrets and privacy

- Secrets live only in the macOS Keychain, through `captain-vault`. They never go in env files,
  shell rc files, command arguments, or logs.
- Wrap secret values in `captain_vault::Secret`, whose `Debug` and `Display` print `<redacted>`.
- Logs carry ids, counts, and types. Message text and captions go to `trace` level only.
- Local state files are written with mode `0600`, atomically (write to a temp file, then rename).

## Async

- Tokio is the only runtime. Blocking calls (Keychain, large file reads) run in
  `tokio::task::spawn_blocking` when they sit on a hot path.
- Every spawned task has an owner that awaits it or cancels it on shutdown.

## Tests

- Unit tests live next to the code in `#[cfg(test)] mod tests`. They never touch the network or
  the real Keychain. Use the in-memory secret store instead.
- Tests that need a real account are `#[ignore]`d and run only with `CAPTAIN_LIVE=1`.

## Commits

- Format: `type(scope): subject`, imperative, no more than 72 characters. Types: `feat`, `fix`,
  `refactor`, `docs`, `test`, `chore`. The scope is the crate short name (`core`, `protocol`,
  `vault`, `format`, `cli`) or `repo`.
- The body explains why. The diff already shows what.

## Prose

Docs, READMEs, and commit bodies follow the `unslop` rules: no em dashes, sentence case headings,
no decorative emoji.
