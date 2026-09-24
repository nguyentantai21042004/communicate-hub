//! Manual tickets from Saved Messages: a `cap:` note plus the messages forwarded with it.
//!
//! TELEGRAM: `messageFwdHeader.saved_from_peer` and `saved_from_msg_id` are only set on
//! forwards into Saved Messages (<https://core.telegram.org/constructor/messageFwdHeader>), so
//! that is the one place a forward still points at its original message.

use std::collections::VecDeque;

use captain_core::{ChatId, MessageId, MessageView};
use serde::Serialize;

/// Prefix that marks a Saved Messages note as a request, compared case-insensitively.
pub const NOTE_PREFIX: &str = "cap:";

/// Original message a forward points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Pointer {
    /// Chat of the original message.
    pub chat: ChatId,
    /// Id of the original message.
    pub message: MessageId,
}

/// A manual request: the owner's note and the messages it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualRequest {
    /// Note text after the prefix.
    pub note: String,
    /// Forwarded originals, in arrival order.
    pub pointers: Vec<Pointer>,
    /// When the note arrived.
    pub at: i64,
}

/// Pairs `cap:` notes with nearby forwards. Everything else in Saved Messages is ignored.
#[derive(Debug)]
pub struct ManualCollector {
    before_secs: i64,
    after_secs: i64,
    recent: VecDeque<(Pointer, i64)>,
    pending: Option<ManualRequest>,
}

impl ManualCollector {
    /// Collector that joins forwards up to `before_secs` before a note and `after_secs` after.
    pub fn new(before_secs: i64, after_secs: i64) -> Self {
        Self {
            before_secs,
            after_secs,
            recent: VecDeque::new(),
            pending: None,
        }
    }

    /// Offers one message the account wrote in Saved Messages. Returns a request that a new
    /// note closed early.
    pub fn observe(&mut self, message: &MessageView, now: i64) -> Option<ManualRequest> {
        self.recent.retain(|(_, at)| now - at <= self.before_secs);
        if let Some(pointer) = pointer(message) {
            match &mut self.pending {
                Some(request) => request.pointers.push(pointer),
                None => self.recent.push_back((pointer, now)),
            }
            return None;
        }
        let note = note_text(&message.text.text)?;
        let closed = self.pending.take().filter(|r| !r.pointers.is_empty());
        self.pending = Some(ManualRequest {
            note,
            pointers: self.recent.drain(..).map(|(p, _)| p).collect(),
            at: now,
        });
        closed
    }

    /// Returns the pending request once no more forwards can join it. A note without any
    /// forward is dropped.
    pub fn due(&mut self, now: i64) -> Option<ManualRequest> {
        let ready = self
            .pending
            .as_ref()
            .is_some_and(|r| now - r.at >= self.after_secs);
        if !ready {
            return None;
        }
        self.pending.take().filter(|r| !r.pointers.is_empty())
    }

    /// When [`Self::due`] will next have something, if a note is pending.
    pub fn next_deadline(&self) -> Option<i64> {
        self.pending.as_ref().map(|r| r.at + self.after_secs)
    }
}

fn pointer(message: &MessageView) -> Option<Pointer> {
    let forward = message.forward.as_ref()?;
    Some(Pointer {
        chat: forward.saved_from_chat?,
        message: forward.saved_from_message?,
    })
}

fn note_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let head = trimmed.get(..NOTE_PREFIX.len())?;
    head.eq_ignore_ascii_case(NOTE_PREFIX)
        .then(|| trimmed[NOTE_PREFIX.len()..].trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use captain_core::{ChatKind, ChatSummary, FormattedText, ForwardOrigin};

    fn saved(text: &str, forwarded_from: Option<(i64, i32)>) -> MessageView {
        MessageView {
            chat: ChatSummary {
                id: ChatId(42),
                kind: ChatKind::User,
                name: None,
                username: None,
                is_bot: false,
            },
            id: MessageId(1),
            sender: None,
            outgoing: true,
            date_unix: 0,
            text: FormattedText::plain(text),
            media: None,
            reply_to: None,
            album_id: None,
            mentioned: false,
            topic: None,
            quote_text: None,
            forward: forwarded_from.map(|(chat, id)| ForwardOrigin {
                from: None,
                date_unix: 0,
                saved_from_chat: Some(ChatId(chat)),
                saved_from_message: Some(MessageId(id)),
            }),
            edited_at_unix: None,
        }
    }

    #[test]
    fn note_then_forwards_become_one_request() {
        let mut c = ManualCollector::new(120, 10);
        assert!(c.observe(&saved("CAP: trả lời giúp", None), 0).is_none());
        c.observe(&saved("", Some((-100, 5))), 1);
        c.observe(&saved("", Some((-100, 6))), 1);
        assert!(c.due(9).is_none());
        let r = c.due(10).unwrap();
        assert_eq!(r.note, "trả lời giúp");
        assert_eq!(r.pointers.len(), 2);
    }

    #[test]
    fn earlier_forwards_join_and_plain_notes_are_ignored() {
        let mut c = ManualCollector::new(120, 10);
        c.observe(&saved("", Some((7, 1))), 0);
        c.observe(&saved("shopping list", None), 5);
        c.observe(&saved("cap:", None), 100);
        let r = c.due(110).unwrap();
        assert_eq!(
            r.pointers,
            [Pointer {
                chat: ChatId(7),
                message: MessageId(1)
            }]
        );
        assert!(c.due(200).is_none());
    }

    #[test]
    fn a_note_without_forwards_is_dropped() {
        let mut c = ManualCollector::new(120, 10);
        c.observe(&saved("cap: nothing attached", None), 0);
        assert!(c.due(10).is_none());
        assert!(c.next_deadline().is_none());
    }
}
