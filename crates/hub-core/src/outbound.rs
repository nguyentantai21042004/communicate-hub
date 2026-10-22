//! A message the bridge sends from Telegram out to a source network.

use serde::{Deserialize, Serialize};

use crate::{Attachment, ConversationId, MessageRef};

/// One message to deliver on the source network named by [`conversation`](Self::conversation).
///
/// `text` is plain text. Telegram HTML is converted before it reaches this struct.
/// An empty `text` with a non-empty `attachments` list is a media-only send. The adapter
/// decides what to do when both are empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// Destination conversation.
    pub conversation: ConversationId,
    /// Plain text body. Empty when the message is media only.
    pub text: String,
    /// Files, stickers, and other non-text parts, in display order.
    pub attachments: Vec<Attachment>,
    /// Source message this one replies to, when the network can thread it.
    pub reply_to: Option<MessageRef>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttachmentKind, AttachmentSource, ConversationKind, Network};

    #[test]
    fn outgoing_message_round_trips() -> serde_json::Result<()> {
        let conversation = ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Dm,
            external_id: "user-1".into(),
        };
        let message = OutgoingMessage {
            conversation: conversation.clone(),
            text: "ping".into(),
            attachments: vec![Attachment {
                kind: AttachmentKind::File,
                mime: None,
                size_bytes: None,
                source: AttachmentSource::LocalPath("/tmp/note.txt".into()),
                file_name: Some("note.txt".into()),
            }],
            reply_to: Some(MessageRef {
                conversation,
                message_id: "m-0".into(),
            }),
        };
        let json = serde_json::to_string(&message)?;
        assert_eq!(serde_json::from_str::<OutgoingMessage>(&json)?, message);
        Ok(())
    }
}
