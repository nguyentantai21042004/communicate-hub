# Review and outcomes (P2)

## Why native drafts

Telegram keeps one cloud draft per chat, and per topic in forums. It syncs to every device and
shows as "Draft:" in the chat list. Writing the reply there means:

- No review UI to build.
- The message goes out from the official app, as Tài, only when he presses Send. API terms 1.4
  ("actions on behalf of the user without the user's knowledge and consent") is satisfied by
  construction.
- A bug in the pipeline can at worst leave a bad draft. It cannot message anyone.

## Writing a draft

1. Read the current draft with `messages.getPeerDialogs` (`dialog.draft`).
2. If a draft exists and it is not the last text this system saved there (compared by hash),
   Tài is typing something. Do not overwrite. Mark the draft `blocked` and mention it in the
   next digest.
3. Otherwise call `messages.saveDraft` with the text, entities from layer 2, and
   `reply_to = inputReplyToMessage{reply_to_msg_id: anchor, top_msg_id: topic}`.
4. Store the text and hash in `drafts`.

A merged ticket may replace its own earlier draft only while that draft is still untouched.

`messages.saveDraft` has a `media` field in layer 229. Whether the apps show and send a media
draft is unknown until P0 checks it; until then drafts are text only.

## Outcome labels

The daemon watches the key after saving:

| Outcome | Detected by |
|---|---|
| `sent_as_is` | Tài's next outgoing message on the key, normalized edit ratio below 0.05 against the draft |
| `sent_edited` | Same, ratio at or above 0.05; the ratio is stored |
| `discarded` | `updateDraftMessage` with an empty draft, and no send |
| `replaced` | `updateDraftMessage` with text that is not ours, then a send |
| `expired` | None of the above within `draft_ttl_secs`, 24 hours by default |

An expired draft is left in place. Clearing text in Tài's chat without him seeing it would be
more surprising than a stale draft.

The race where Tài starts typing on his phone just as the daemon writes is narrowed by step 2
but not closed. P0 checks what the apps do when the cloud draft changes under an open editor.
