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
    /// The account read a chat up to `max_id`, on any of its devices.
    ReadInbox {
        /// Chat that was read.
        chat: ChatId,
        /// Forum topic or comment thread, when Telegram reports one.
        topic: Option<MessageId>,
        /// Highest message id now read.
        max_id: MessageId,
        /// Messages still unread afterwards, when Telegram reports it.
        still_unread: Option<i32>,
    },
    /// Any other update (typing, outgoing read receipts, online status). Only `raw` describes it.
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
    /// Message this one replies to. In a forum this is `None` for a plain message in a topic,
    /// even though Telegram points its reply header at the topic's first message.
    pub reply_to: Option<MessageId>,
    /// Shared by every message of one album; `None` for standalone messages.
    pub album_id: Option<i64>,
    /// Whether the message mentions this account, by `@username` or by name.
    #[serde(default)]
    pub mentioned: bool,
    /// Forum topic the message belongs to, as the id of the topic's first message. `None`
    /// outside forums and in a forum's General topic.
    #[serde(default)]
    pub topic: Option<MessageId>,
    /// The part of the replied-to message the sender quoted, if any.
    #[serde(default)]
    pub quote_text: Option<String>,
    /// Where a forwarded message came from.
    #[serde(default)]
    pub forward: Option<ForwardOrigin>,
    /// Last edit time, Unix seconds.
    #[serde(default)]
    pub edited_at_unix: Option<i64>,
}

/// Origin of a forwarded message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardOrigin {
    /// Original sender, when they allow linking forwards to their account.
    pub from: Option<ChatId>,
    /// Original send time, Unix seconds.
    pub date_unix: i64,
    /// Chat the original message is in. Telegram sets it only on forwards into Saved Messages.
    pub saved_from_chat: Option<ChatId>,
    /// Original message id in `saved_from_chat`.
    pub saved_from_message: Option<MessageId>,
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
    /// Whether this is a bot account.
    #[serde(default)]
    pub is_bot: bool,
}

/// One entry of the account's chat list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogSummary {
    /// The chat.
    pub chat: ChatSummary,
    /// Whether the chat is a forum with topics.
    pub forum: bool,
    /// Unread messages.
    pub unread_count: i32,
    /// Time of the last message, Unix seconds.
    pub last_message_unix: Option<i64>,
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
