//! Telegram forum input that `hub-tgbot` will hand to the bridge.
//!
//! This crate defines the struct. `hub-tgbot` produces it. The JSON shape is an internally
//! tagged object with `"type"` and `snake_case` names, matching [`hub_core::RemoteEvent`].

use hub_core::{Attachment, TgMessageId, TopicId};
use serde::{Deserialize, Serialize};

/// One update typed in a mapped forum topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TopicIncoming {
    /// A person sent a message in the topic.
    Message {
        /// Forum topic the message was posted in.
        topic: TopicId,
        /// Bot API message id of that post.
        tg_message: TgMessageId,
        /// Plain text. Empty when the message is attachments only.
        text: String,
        /// Files and other non-text parts, in display order.
        attachments: Vec<Attachment>,
        /// The post this one replies to, when Telegram threaded it.
        reply_to: Option<TgMessageId>,
    },
    /// A person added or removed a reaction on a topic message.
    Reaction {
        /// Forum topic that holds the message.
        topic: TopicId,
        /// Bot API message id that was reacted to.
        tg_message: TgMessageId,
        /// Reaction emoji, as Telegram reported it.
        emoji: String,
        /// `true` when the emoji was applied, `false` when it was removed.
        added: bool,
    },
    /// A person edited the text of a topic message.
    ///
    /// The source-network traits have no edit call. The bridge sends a new message that
    /// replies to the mapped network message, when that mapping exists.
    Edit {
        /// Forum topic that holds the message.
        topic: TopicId,
        /// Bot API message id that was edited.
        tg_message: TgMessageId,
        /// New plain text.
        text: String,
    },
    /// A person deleted a topic message.
    Unsend {
        /// Forum topic that held the message.
        topic: TopicId,
        /// Bot API message id that was deleted.
        tg_message: TgMessageId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_incoming_matches_the_remote_event_json_shape() {
        let message = TopicIncoming::Message {
            topic: TopicId(1),
            tg_message: TgMessageId(10),
            text: "hi".into(),
            attachments: Vec::new(),
            reply_to: None,
        };
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"type":"message","topic":1,"tg_message":10,"text":"hi","attachments":[],"reply_to":null}"#
        );

        let reaction = TopicIncoming::Reaction {
            topic: TopicId(1),
            tg_message: TgMessageId(10),
            emoji: "👍".into(),
            added: true,
        };
        assert_eq!(
            serde_json::to_string(&reaction).unwrap(),
            r#"{"type":"reaction","topic":1,"tg_message":10,"emoji":"👍","added":true}"#
        );

        let edit = TopicIncoming::Edit {
            topic: TopicId(1),
            tg_message: TgMessageId(10),
            text: "new".into(),
        };
        assert_eq!(
            serde_json::to_string(&edit).unwrap(),
            r#"{"type":"edit","topic":1,"tg_message":10,"text":"new"}"#
        );

        let unsend = TopicIncoming::Unsend {
            topic: TopicId(1),
            tg_message: TgMessageId(10),
        };
        assert_eq!(
            serde_json::to_string(&unsend).unwrap(),
            r#"{"type":"unsend","topic":1,"tg_message":10}"#
        );
    }
}
