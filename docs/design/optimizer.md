# Optimizer (P3)

## Responsibility

Improve the trigger and pull policy from evidence, without changing anything by itself.

## Inputs

Per ticket: trigger kind, route, burst shape, implicit and explicit labels. Per run: messages
returned, `used_ids`, tool calls, time, tokens, cost, outcome.

## Measures

| Measure | Meaning |
|---|---|
| Trigger precision | Tickets that led to a sent draft or a reply, over all tickets, per trigger kind |
| Trigger recall | Messages Tài answered that some ticket covered, over all messages Tài answered in scope |
| Pull efficiency | `used_ids` over messages returned |
| Under-pull | `sent_edited` with a high edit ratio, or the runner saying context was missing |
| Draft acceptance | `sent_as_is` plus `sent_edited`, over drafts |

## Output

`captain report --propose` prints the measures for the period and a diff against
`policy.toml` and `scope.toml`: new debounce values, budgets, triggers to demote to digest,
chats to add from `gate_drops`. Tài applies the diff by hand, or not. Nothing changes at
runtime.

Offline because volume is a few dozen tickets a day and labels are noisy. With that little
data, online tuning would overfit to one unusual week.

## Open

The sketch says "Optimizer/Jev". If Jev names a specific tool or method, this is where it would
plug in: it would consume the same run and outcome tables and produce the same kind of diff.
