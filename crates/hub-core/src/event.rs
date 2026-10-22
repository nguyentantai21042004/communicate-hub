//! Events a source network emits, normalized so the bridge never sees sidecar types.
//!
//! [`RemoteEvent`] is internally tagged: JSON objects carry `"type"` and `snake_case` names.
//! [`RemoteEvent::Message`] and [`RemoteEvent::Edit`] are newtype variants, so their fields
//! sit next to `type` rather than under a nested object.

use serde::{Deserialize, Serialize};

use crate::{ConversationId, MessageRef, SessionState};

/// One normalized update from a source network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteEvent {
    /// A new message arrived.
    Message(RemoteMessage),
    /// An existing message's text or attachments were replaced.
    Edit(RemoteEdit),
    /// The sender removed a message.
    Unsend {
        /// Message that was removed.
        target: MessageRef,
    },
    /// A reaction was added or removed.
    Reaction {
        /// Message that was reacted to.
        target: MessageRef,
        /// Reaction emoji, as the source reported it.
        emoji: String,
        /// `true` when the emoji was applied, `false` when it was removed.
        added: bool,
    },
    /// The conversation title or avatar changed.
    ///
    /// `None` means that field did not change. It does not mean the field was cleared.
    ChatInfo {
        /// Conversation whose profile changed.
        conversation: ConversationId,
        /// New title, when the title changed.
        title: Option<String>,
        /// New avatar, when the avatar changed.
        avatar: Option<AttachmentSource>,
    },
    /// A membership notice, already rendered as text by the adapter.
    GroupEvent {
        /// Group the notice belongs to.
        conversation: ConversationId,
        /// Sentence to mirror, for example a join or leave line.
        text: String,
        /// When the notice happened, as Unix seconds.
        sent_at_unix_secs: i64,
    },
    /// Someone asked to connect, before a conversation exists.
    FriendRequest {
        /// Requester id on the source network.
        sender_id: String,
        /// Requester name shown by the source client.
        sender_display_name: String,
        /// Greeting attached to the request. Empty when they sent none.
        message: String,
    },
    /// The source account's session changed state.
    SessionChanged {
        /// New session state.
        state: SessionState,
    },
}

/// A new message on a source network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMessage {
    /// Conversation the message belongs to.
    pub conversation: ConversationId,
    /// Message id assigned by the network.
    pub message_id: String,
    /// Sender id on that network.
    pub sender_id: String,
    /// Sender name shown by the source client.
    pub sender_display_name: String,
    /// Plain text body. Empty when the message is attachments only.
    pub text: String,
    /// Non-text parts, in display order.
    pub attachments: Vec<Attachment>,
    /// Message this one replies to, when the source reported a reply.
    pub reply_to: Option<MessageRef>,
    /// When the sender sent it, as Unix seconds.
    pub sent_at_unix_secs: i64,
    /// People named in the body.
    pub mentions: Vec<Mention>,
}

/// Replacement body of an existing message.
///
/// `text` and `attachments` are the full new body, not a patch. An empty `attachments` list
/// means the edited message has no attachments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEdit {
    /// Message that was edited.
    pub target: MessageRef,
    /// New plain text body.
    pub text: String,
    /// New attachments, in display order.
    pub attachments: Vec<Attachment>,
    /// When the edit happened, as Unix seconds.
    pub edited_at_unix_secs: i64,
}

/// A person named in a [`RemoteMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// User id on the source network.
    pub user_id: String,
    /// Name shown in the source message.
    pub display_name: String,
}

/// One non-text part of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// How the bridge should present the bytes.
    pub kind: AttachmentKind,
    /// MIME type, when the source reported one.
    pub mime: Option<String>,
    /// Size in bytes, when the source reported one.
    pub size_bytes: Option<u64>,
    /// Where to read the bytes, or a URL that stands in for a non-file kind.
    pub source: AttachmentSource,
    /// File name shown to the recipient, when the source reported one.
    pub file_name: Option<String>,
}

/// Kind of an [`Attachment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A still image shown inline.
    Photo,
    /// A video.
    Video,
    /// A voice note.
    Voice,
    /// An audio file that is not a voice note.
    Audio,
    /// A generic file.
    File,
    /// A sticker.
    Sticker,
    /// An animated GIF.
    Gif,
    /// A map location. `source` is a URL the adapter chose for it.
    Location,
    /// A poll. The question lives in the message text.
    Poll,
    /// A bank card share.
    BankCard,
    /// A shared contact.
    Contact,
}

/// Where an [`Attachment`]'s bytes live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSource {
    /// An `https` or other URL the bridge can fetch.
    Url(String),
    /// A filesystem path on this machine.
    LocalPath(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConversationKind, Network};

    fn conversation(network: Network, kind: ConversationKind, external_id: &str) -> ConversationId {
        ConversationId {
            network,
            kind,
            external_id: external_id.to_owned(),
        }
    }

    fn message_ref(conversation: ConversationId, message_id: &str) -> MessageRef {
        MessageRef {
            conversation,
            message_id: message_id.to_owned(),
        }
    }

    fn assert_wire(event: &RemoteEvent, json: &str) -> serde_json::Result<()> {
        assert_eq!(serde_json::to_string(event)?, json);
        assert_eq!(&serde_json::from_str::<RemoteEvent>(json)?, event);
        Ok(())
    }

    #[test]
    fn every_remote_event_matches_its_wire_string() -> serde_json::Result<()> {
        let zalo_dm = conversation(Network::Zalo, ConversationKind::Dm, "user-1");
        let zalo_group = conversation(Network::Zalo, ConversationKind::Group, "g1");

        assert_wire(
            &RemoteEvent::Message(RemoteMessage {
                conversation: zalo_dm.clone(),
                message_id: "m-1".into(),
                sender_id: "user-1".into(),
                sender_display_name: "An".into(),
                text: "hello".into(),
                attachments: vec![Attachment {
                    kind: AttachmentKind::Photo,
                    mime: Some("image/jpeg".into()),
                    size_bytes: Some(12),
                    source: AttachmentSource::Url("https://cdn.example/a.jpg".into()),
                    file_name: Some("a.jpg".into()),
                }],
                reply_to: Some(message_ref(zalo_dm, "m-0")),
                sent_at_unix_secs: 1_700_000_000,
                mentions: vec![Mention {
                    user_id: "user-2".into(),
                    display_name: "Binh".into(),
                }],
            }),
            concat!(
                r#"{"type":"message","conversation":"zalo:dm:user-1","message_id":"m-1","#,
                r#""sender_id":"user-1","sender_display_name":"An","text":"hello","#,
                r#""attachments":[{"kind":"photo","mime":"image/jpeg","size_bytes":12,"#,
                r#""source":{"url":"https://cdn.example/a.jpg"},"file_name":"a.jpg"}],"#,
                r#""reply_to":{"conversation":"zalo:dm:user-1","message_id":"m-0"},"#,
                r#""sent_at_unix_secs":1700000000,"mentions":[{"user_id":"user-2","display_name":"Binh"}]}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::Edit(RemoteEdit {
                target: message_ref(zalo_group.clone(), "m-2"),
                text: "edited".into(),
                attachments: vec![],
                edited_at_unix_secs: 1_700_000_001,
            }),
            concat!(
                r#"{"type":"edit","target":{"conversation":"zalo:group:g1","message_id":"m-2"},"#,
                r#""text":"edited","attachments":[],"edited_at_unix_secs":1700000001}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::Unsend {
                target: message_ref(
                    conversation(Network::Messenger, ConversationKind::Dm, "psid"),
                    "mid",
                ),
            },
            concat!(
                r#"{"type":"unsend","target":{"conversation":"messenger:dm:psid","#,
                r#""message_id":"mid"}}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::Reaction {
                target: message_ref(
                    conversation(Network::Telegram, ConversationKind::Dm, "42"),
                    "7",
                ),
                emoji: "heart".into(),
                added: false,
            },
            concat!(
                r#"{"type":"reaction","target":{"conversation":"telegram:dm:42","#,
                r#""message_id":"7"},"emoji":"heart","added":false}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::ChatInfo {
                conversation: zalo_group.clone(),
                title: Some("Team".into()),
                avatar: Some(AttachmentSource::LocalPath("/tmp/avatar.jpg".into())),
            },
            concat!(
                r#"{"type":"chat_info","conversation":"zalo:group:g1","title":"Team","#,
                r#""avatar":{"local_path":"/tmp/avatar.jpg"}}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::GroupEvent {
                conversation: zalo_group,
                text: "An joined".into(),
                sent_at_unix_secs: 1_700_000_002,
            },
            concat!(
                r#"{"type":"group_event","conversation":"zalo:group:g1","text":"An joined","#,
                r#""sent_at_unix_secs":1700000002}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::FriendRequest {
                sender_id: "user-9".into(),
                sender_display_name: "Chi".into(),
                message: "add me".into(),
            },
            concat!(
                r#"{"type":"friend_request","sender_id":"user-9","#,
                r#""sender_display_name":"Chi","message":"add me"}"#
            ),
        )?;

        assert_wire(
            &RemoteEvent::SessionChanged {
                state: SessionState::Expired,
            },
            r#"{"type":"session_changed","state":"expired"}"#,
        )?;
        Ok(())
    }

    #[test]
    fn attachment_kinds_use_snake_case_wire_names() -> serde_json::Result<()> {
        let kinds = [
            (AttachmentKind::Photo, "photo"),
            (AttachmentKind::Video, "video"),
            (AttachmentKind::Voice, "voice"),
            (AttachmentKind::Audio, "audio"),
            (AttachmentKind::File, "file"),
            (AttachmentKind::Sticker, "sticker"),
            (AttachmentKind::Gif, "gif"),
            (AttachmentKind::Location, "location"),
            (AttachmentKind::Poll, "poll"),
            (AttachmentKind::BankCard, "bank_card"),
            (AttachmentKind::Contact, "contact"),
        ];
        for (kind, wire) in kinds {
            assert_eq!(serde_json::to_string(&kind)?, format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<AttachmentKind>(&format!("\"{wire}\""))?,
                kind
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_event_json_is_rejected() {
        assert!(serde_json::from_str::<RemoteEvent>(r#"{"type":"typing"}"#).is_err());
        assert!(
            serde_json::from_str::<RemoteEvent>(
                r#"{"type":"unsend","target":{"conversation":"nope","message_id":"1"}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<RemoteEvent>(r#"{"type":"session_changed","state":"nope"}"#)
                .is_err()
        );
    }
}
