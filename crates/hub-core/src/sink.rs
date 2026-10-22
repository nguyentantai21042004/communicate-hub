//! The Telegram forum side of the bridge.
//!
//! `hub-tgbot` implements [`TopicSink`] on the Bot API. The bridge is generic over the trait,
//! so tests pass a fake instead of Telegram. Methods take `&self` and return `impl Future + Send`.
//! Implement each method with `async fn`.

use crate::{Attachment, Result, TgMessageId, TopicId};

/// Telegram forum topics the bridge writes into.
///
/// `text_html` is Telegram HTML and is passed through. This trait does not parse markup.
/// Callers compare text and captions against [`crate::limits`] before calling.
pub trait TopicSink: Send + Sync {
    /// Opens a forum topic and returns its id.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure.
    fn create_topic(&self, title: String) -> impl Future<Output = Result<TopicId>> + Send;

    /// Closes a forum topic. Does not delete the topic or its messages.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure.
    fn close_topic(&self, topic: TopicId) -> impl Future<Output = Result<()>> + Send;

    /// Posts HTML into a topic and returns the Telegram message id.
    ///
    /// `reply_to` threads the post under an earlier message in the same topic.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure, including a text over the Telegram limit.
    fn send_text(
        &self,
        topic: TopicId,
        text_html: String,
        reply_to: Option<TgMessageId>,
    ) -> impl Future<Output = Result<TgMessageId>> + Send;

    /// Posts one attachment into a topic and returns the Telegram message id.
    ///
    /// `caption` is plain text. `None` means the message has no caption.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure, including a caption over the Telegram limit.
    fn send_media(
        &self,
        topic: TopicId,
        attachment: Attachment,
        caption: Option<String>,
    ) -> impl Future<Output = Result<TgMessageId>> + Send;

    /// Sets the reaction on a topic message, or clears it when `emoji` is `None`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure.
    fn set_reaction(
        &self,
        topic: TopicId,
        tg_message: TgMessageId,
        emoji: Option<String>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Deletes one message in a topic.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure.
    fn delete_message(
        &self,
        topic: TopicId,
        tg_message: TgMessageId,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Renames a forum topic.
    ///
    /// The default returns [`crate::Error::Unsupported`] so an implementor that has no
    /// rename keeps compiling. `hub-tgbot` overrides it with `editForumTopic`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Unsupported`] when the implementor has no rename.
    /// Returns [`crate::Error::TopicGone`] when `topic` does not exist.
    /// Returns [`crate::Error::RateLimited`] when Telegram asks the caller to wait.
    /// Returns [`crate::Error::Adapter`] for any other failure.
    fn rename_topic(
        &self,
        topic: TopicId,
        title: String,
    ) -> impl Future<Output = Result<()>> + Send {
        async move {
            let _ = (topic, title);
            Err(crate::Error::Unsupported)
        }
    }
}
