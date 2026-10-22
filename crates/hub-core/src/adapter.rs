//! The source-network side of the bridge.
//!
//! `hub-zalo` implements [`NetworkAdapter`]. A Messenger adapter implements the same trait.
//! Methods take `&self` so one shared handle can wait for events and send. Implementors put
//! their own lock around the socket or sidecar.

use crate::{MessageRef, ModuleHealth, Network, OutgoingMessage, RemoteEvent, Result};

/// A logged-in source network that can receive events and send as the account.
///
/// The bridge is generic over this trait. Methods return `impl Future + Send` so that generic
/// caller can run on a Tokio task. Implement each method with `async fn`.
pub trait NetworkAdapter: Send + Sync {
    /// Network this handle speaks.
    fn network(&self) -> Network;

    /// Waits for the next event.
    ///
    /// Returns `Ok(None)` once the adapter has shut down cleanly.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Adapter`] when the source connection fails.
    fn next_event(&self) -> impl Future<Output = Result<Option<RemoteEvent>>> + Send;

    /// Sends one message and returns the id the network assigned.
    ///
    /// The returned [`MessageRef::conversation`](MessageRef::conversation) is the destination
    /// conversation. `message_id` is the new remote id, not an id the caller chose.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Adapter`] when the network rejects the send.
    fn send(&self, message: OutgoingMessage) -> impl Future<Output = Result<MessageRef>> + Send;

    /// Applies or removes `emoji` on `target`.
    ///
    /// `added` is `true` to apply the emoji and `false` to remove it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Adapter`] when the network rejects the reaction.
    fn react(
        &self,
        target: MessageRef,
        emoji: String,
        added: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Removes a message this account sent.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Adapter`] when the network rejects the removal.
    fn unsend(&self, target: MessageRef) -> impl Future<Output = Result<()>> + Send;

    /// Reports the adapter's current health.
    ///
    /// A session that was reached but is unusable is `Ok` with [`crate::ModuleState::Down`],
    /// not an error.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Adapter`] when the probe itself cannot run.
    fn health(&self) -> impl Future<Output = Result<ModuleHealth>> + Send;
}
