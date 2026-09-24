//! What the account receives, normalized, with Telegram's raw payload kept alongside.

use serde::{Deserialize, Serialize};

use crate::{ChatId, FormattedText, MessageId};

/// One update from Telegram, as seen by the logged-in account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboundEvent {
    /// Local receive time, Unix seconds.
    pub received_at_unix: i64,
    /// Normalized meaning of the update.
    pub kind: EventKind,
    /// Variant name of the raw TL update enum, for example `NewMessage` for `updateNewMessage`.
    pub raw_type: String,
    /// The raw TL object as JSON. Field names follow the TL schema.
    pub raw: serde_json::Value,
}

/// Normalized meaning of an [`InboundEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A new message in any chat, including ones this account sent from another device.
    MessageNew(MessageView),
    /// An existing message was edited.
    MessageEdited(MessageView),
    /// Messages were deleted. `chat` is `None` for private chats and basic groups, where
    /// Telegram does not say which chat the ids belonged to.
    MessagesDeleted {
        /// Chat the ids belong to, when Telegram reports it.
        chat: Option<ChatId>,
        /// Deleted message ids.
        ids: Vec<MessageId>,
    },
    /// Any other update (typing, read receipts, online status). Only `raw` describes it.
    Other,
}

/// A message, reduced to what an application needs to react to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageView {
    /// Chat the message is in.
    pub chat: ChatSummary,
    /// Message id within the chat.
    pub id: MessageId,
    /// Sender, when known. Channel posts may have none.
    pub sender: Option<ChatSummary>,
    /// Whether this account sent it (from any device).
    pub outgoing: bool,
    /// Send time, Unix seconds.
    pub date_unix: i64,
    /// Text or caption, with styling.
    pub text: FormattedText,
    /// Attached media, if any.
    pub media: Option<MediaSummary>,
    /// Message this one replies to.
    pub reply_to: Option<MessageId>,
    /// Shared by every message of one album; `None` for standalone messages.
    pub album_id: Option<i64>,
}

/// Identity of a chat or user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatSummary {
    /// Marked id.
    pub id: ChatId,
    /// Kind of chat.
    pub kind: ChatKind,
    /// Display name or title, when cached.
    pub name: Option<String>,
    /// Public username without `@`, when it has one.
    pub username: Option<String>,
}

/// Kind of a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    /// One-to-one chat with a user or bot.
    User,
    /// Basic group.
    Group,
    /// Channel or supergroup.
    Channel,
}

/// Media attached to a message. Download it through the protocol layer by chat and message id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSummary {
    /// Variant name of the raw TL media enum, for example `Photo` for `messageMediaPhoto`.
    pub raw_type: String,
    /// MIME type, when Telegram reports one.
    pub mime_type: Option<String>,
    /// Original file name, for documents.
    pub file_name: Option<String>,
    /// Size in bytes, when known.
    pub size_bytes: Option<u64>,
    /// Whether the media is hidden behind a spoiler.
    pub spoiler: bool,
}
