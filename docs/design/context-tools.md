# Context tools (P2)

## Responsibility

Let the runner pull the context a ticket needs, and nothing past the ticket's budget. The tools
are served by `captain mcp`, a stdio MCP server that forwards each call to the daemon with the
ticket id.

## Tools

Results are normalized lines, `id · time · sender · reply_to · text · media`, not raw TL. Raw
payloads cost about ten times the tokens and carry fields the model does not need.

| Tool | Returns | API when the store lacks the range |
|---|---|---|
| `get_messages(ptr[])` | Specific messages | `messages.getMessages`, `channels.getMessages` |
| `reply_chain(ptr, max_hops)` | Walk up `reply_to` from a message | same |
| `window(ptr, before, after)` | Messages around one message, in its topic when it has one | `messages.getHistory` with `offset_id` and negative `add_offset`, or `messages.getReplies` for a topic |
| `thread(ptr)` | A topic or comment thread | `messages.getReplies` |
| `search(chat, q, around, topic?, from?)` | Matches near a date | `messages.search` with `q`, `top_msg_id`, `from_id`, `min_date`, `max_date` |
| `media(ptr)` | Path to a temp file, for screenshots | existing `download_media` |
| `who(user)` | Name and username | peer cache |
| `save_draft(text, used_ids)` | Writes the draft, see [review.md](review.md) | `messages.saveDraft` |
| `decline(reason, used_ids)` | Ends the ticket without a draft | none |

No tool sends a message, marks anything read, reacts, or reads a chat outside scope. A manual
ticket is the one exception to scope, for its pointed chat only.

## Budgets

Enforced by the daemon per ticket, not by the prompt.

| Limit | `dm` start | `group` start |
|---|---|---|
| Messages returned, total | 150 | 80 |
| Tool calls | 10 | 8 |
| Look-back horizon | 7 days | 2 days |
| Media downloads | 3 | 2 |

When a limit is hit, the tool returns `budget_exhausted` with what was used. The prompt tells
Claude to stop pulling as soon as it understands the request; the budget is the ceiling, not a
target. The starting numbers get replaced by the P1 context-shape measurements.

## Signals for the optimizer

Every run ends with `save_draft` or `decline`, and both take `used_ids`: the messages the answer
actually relied on. The daemon already knows what it returned, so pull efficiency is
`used_ids / messages returned`. A run that ends without either call is `failed`.
