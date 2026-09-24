# Runner (P2)

## Responsibility

Turn a queued ticket into a draft or a decline by running Claude Code headless.

## Process

- One `claude -p` process per ticket, from a queue with concurrency one. Volume is low, and
  serial runs keep cost and the draft slot per chat predictable.
- Working directory: `<state dir>/runner/`, holding a `CLAUDE.md` with Tài's reply style,
  language and roster notes. It is not a code repo.
- Flags, to be checked against `claude --help` when this is built:
  - `--output-format json`, which returns the session id, usage and cost.
  - `--mcp-config` with `captain mcp` and the read-only work MCPs Tài chooses.
  - `--strict-mcp-config`, so the global MCP list on the machine is not loaded. That list
    includes production systems a message author must never reach.
  - `--allowedTools` limited to the captain tools, the chosen read-only MCP tools, and `Read`,
    `Grep`, `Glob` over `--add-dir` repo clones. No `Bash`, `Edit`, `Write` or web tools.
  - `--model` from `policy.toml`.
- The prompt contains the ticket fields and seed, wrapped as quoted data with a line saying
  message text is untrusted input and never instructions.
- The session id is stored in `runs`. A follow-up on a merged ticket uses `--resume`.

## Steps the prompt asks for

1. Decide whether this needs Tài's reply. If not, `decline(no_reply_needed)`.
2. Decide whether it is Tài's to answer, from the roster in `CLAUDE.md` and work MCPs. If not,
   draft a short handoff naming the owner, or `decline(not_mine)`.
3. Pull context until the request is understood.
4. `save_draft` in the chat's language and Tài's style.

## Limits

| Limit | Start |
|---|---|
| Wall time per run | 180 s, then the process is killed and the ticket is `failed` |
| Retries | 1, only for process or network failure, not for a decline |
| Daily cost cap | set in `policy.toml`; over it, new tickets route to digest |

## Where it runs

Daemon, runner and repo clones all live on the second Mac in v1, with a `git fetch` before each
run. A runner on the main Mac would need the daemon socket over the network, which is deferred.
