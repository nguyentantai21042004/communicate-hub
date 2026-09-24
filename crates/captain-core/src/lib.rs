//! Protocol-agnostic types shared by every layer of telegram-captain.
//!
//! This crate owns the vocabulary (chats, messages, formatted text, media) and the limits
//! Telegram imposes on them. It does no I/O and depends on no Telegram library, so a second
//! protocol adapter (for example TDLib) can reuse it unchanged.

pub mod error;
pub mod ids;
pub mod inbound;
pub mod limits;
pub mod outbound;
pub mod protocol;
pub mod text;

pub use error::{Error, Result};
pub use ids::{ChatId, ChatRef, MessageId};
pub use inbound::{
    ChatKind, ChatSummary, DialogSummary, EventKind, ForwardOrigin, InboundEvent, MediaSummary,
    MessageView,
};
pub use outbound::{
    Content, MediaItem, MediaKind, MediaSource, OutgoingMessage, PhotoQuality, SendOptions,
    SentMessage,
};
pub use protocol::Protocol;
pub use text::{EntityKind, FormattedText, TextEntity};
