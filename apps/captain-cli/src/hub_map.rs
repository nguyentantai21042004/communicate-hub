//! Maps `hub-tgbot` forum updates onto `hub-bridge` updates.
//!
//! The two `TopicIncoming` enums are different types. Albums share `media_group_id` and become
//! one message. A reaction often has no topic. `file_id` cannot live on `hub_core::Attachment`,
//! so a file is marked `tg-file:` and [`prepare_outgoing`] turns those into a text note before
//! the bridge sends them to Zalo.

use hub_bridge::TopicIncoming as BridgeIncoming;
use hub_core::{Attachment, AttachmentSource, TgMessageId, TopicId};
use hub_tgbot::{IncomingAttachment, IncomingMessage, TopicIncoming as BotIncoming};

/// Prefix on [`AttachmentSource::Url`] for a Bot API `file_id`.
pub const TG_FILE_PREFIX: &str = "tg-file:";

/// One bot update after album parts are still separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappedUpdate {
    /// A post that already has a topic.
    Message(BridgeIncoming),
    /// A reaction. `topic` is `None` when Telegram did not send a thread.
    Reaction {
        /// Forum topic, when the update had one.
        topic: Option<TopicId>,
        /// Supergroup message that was reacted to.
        tg_message: TgMessageId,
        /// Emoji that changed.
        emoji: String,
        /// `true` when the emoji was added.
        added: bool,
    },
}

/// Converts one bot update. Album parts are not merged here.
pub fn map_update(update: BotIncoming) -> MappedUpdate {
    match update {
        BotIncoming::Message(message) => MappedUpdate::Message(message_to_bridge(message)),
        BotIncoming::Reaction(reaction) => MappedUpdate::Reaction {
            topic: reaction.topic,
            tg_message: reaction.message,
            emoji: reaction.emoji,
            added: reaction.added,
        },
    }
}

/// One album item, in Telegram's order, becomes a single bridge message.
///
/// The first item keeps the text, the reply, and the Telegram message id. Attachments are
/// concatenated.
pub fn merge_album(parts: Vec<IncomingMessage>) -> BridgeIncoming {
    let mut parts = parts.into_iter();
    let Some(first) = parts.next() else {
        return BridgeIncoming::Message {
            topic: TopicId(1),
            tg_message: TgMessageId(0),
            text: String::new(),
            attachments: Vec::new(),
            reply_to: None,
        };
    };
    let mut incoming = message_to_bridge(first);
    let BridgeIncoming::Message { attachments, .. } = &mut incoming else {
        return incoming;
    };
    for part in parts {
        attachments.extend(part.attachments.into_iter().map(attachment_from));
    }
    incoming
}

/// Replaces `tg-file:` attachments with a text note.
///
/// Zalo uploads a local path. A Bot API file id is not one, and sending it would fail in a loop.
pub fn prepare_outgoing(incoming: BridgeIncoming) -> BridgeIncoming {
    let BridgeIncoming::Message {
        topic,
        tg_message,
        text,
        attachments,
        reply_to,
    } = incoming
    else {
        return incoming;
    };
    let mut kept = Vec::new();
    let mut names = Vec::new();
    for attachment in attachments {
        if let AttachmentSource::Url(url) = &attachment.source
            && let Some(file_id) = url.strip_prefix(TG_FILE_PREFIX)
        {
            names.push(if let Some(name) = attachment.file_name.clone() {
                name
            } else {
                file_id.to_owned()
            });
            continue;
        }
        kept.push(attachment);
    }
    if names.is_empty() {
        return BridgeIncoming::Message {
            topic,
            tg_message,
            text,
            attachments: kept,
            reply_to,
        };
    }
    let note = format!("[files: {}]", names.join(", "));
    let text = if text.is_empty() {
        note
    } else {
        format!("{text}\n{note}")
    };
    BridgeIncoming::Message {
        topic,
        tg_message,
        text,
        attachments: kept,
        reply_to,
    }
}

fn message_to_bridge(message: IncomingMessage) -> BridgeIncoming {
    BridgeIncoming::Message {
        topic: message.topic,
        tg_message: message.message,
        text: message.text,
        attachments: message
            .attachments
            .into_iter()
            .map(attachment_from)
            .collect(),
        reply_to: message.reply_to,
    }
}

fn attachment_from(attachment: IncomingAttachment) -> Attachment {
    Attachment {
        kind: attachment.kind,
        mime: attachment.mime,
        size_bytes: attachment.size_bytes,
        source: AttachmentSource::Url(format!("{TG_FILE_PREFIX}{}", attachment.file_id)),
        file_name: attachment.file_name,
    }
}

#[cfg(test)]
mod tests {
    use hub_core::AttachmentKind;
    use hub_tgbot::IncomingReaction;

    use super::*;

    fn photo(file_id: &str, name: &str) -> IncomingAttachment {
        IncomingAttachment {
            kind: AttachmentKind::Photo,
            file_id: file_id.to_owned(),
            file_name: Some(name.to_owned()),
            mime: Some("image/jpeg".into()),
            size_bytes: Some(10),
            duration_secs: Some(3),
        }
    }

    fn post(
        id: i32,
        text: &str,
        group: Option<&str>,
        files: Vec<IncomingAttachment>,
    ) -> IncomingMessage {
        IncomingMessage {
            topic: TopicId(9),
            message: TgMessageId(id),
            sender_id: 7,
            sender_display_name: "An".into(),
            text: text.to_owned(),
            attachments: files,
            reply_to: Some(TgMessageId(3)),
            sent_at_unix_secs: 10,
            media_group_id: group.map(str::to_owned),
        }
    }

    #[test]
    fn album_parts_become_one_message_and_files_become_a_note() {
        let merged = merge_album(vec![
            post(1, "cap", Some("g"), vec![photo("a", "a.jpg")]),
            post(2, "", Some("g"), vec![photo("b", "b.jpg")]),
        ]);
        let prepared = prepare_outgoing(merged);
        let BridgeIncoming::Message {
            topic,
            tg_message,
            text,
            attachments,
            reply_to,
        } = prepared
        else {
            assert!(matches!(prepared, BridgeIncoming::Message { .. }));
            return;
        };
        assert_eq!(topic, TopicId(9));
        assert_eq!(tg_message, TgMessageId(1));
        assert_eq!(reply_to, Some(TgMessageId(3)));
        assert!(attachments.is_empty());
        assert!(text.contains("cap"));
        assert!(text.contains("[files: a.jpg, b.jpg]"));
    }

    #[test]
    fn reaction_without_a_topic_stays_unrouted() {
        let mapped = map_update(BotIncoming::Reaction(IncomingReaction {
            topic: None,
            message: TgMessageId(30),
            user_id: 7,
            emoji: "👍".into(),
            added: true,
        }));
        match mapped {
            MappedUpdate::Reaction {
                topic,
                tg_message,
                emoji,
                added,
            } => {
                assert_eq!(topic, None);
                assert_eq!(tg_message, TgMessageId(30));
                assert_eq!(emoji, "👍");
                assert!(added);
            }
            MappedUpdate::Message(_) => assert!(matches!(mapped, MappedUpdate::Reaction { .. })),
        }
    }
}
