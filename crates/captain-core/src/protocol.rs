//! The contract every protocol adapter implements.

use crate::{ChatId, InboundEvent, MessageId, OutgoingMessage, SentMessage};

/// A logged-in Telegram connection that can receive updates and send as the account.
///
/// `captain-protocol` implements it on MTProto. Callers depend on this trait so a different
/// adapter (TDLib, a test double) can replace it.
pub trait Protocol {
    /// Adapter-specific error.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Waits for the next update. Returns `Ok(None)` once the connection has shut down.
    fn next_event(
        &mut self,
    ) -> impl Future<Output = Result<Option<InboundEvent>, Self::Error>> + Send;

    /// Validates and sends one message or album. An album returns one entry per item.
    fn send(
        &self,
        message: OutgoingMessage,
    ) -> impl Future<Output = Result<Vec<SentMessage>, Self::Error>> + Send;

    /// Downloads the media attached to a message, or `Ok(None)` if it has none.
    fn download_media(
        &self,
        chat: ChatId,
        message: MessageId,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send;
}
