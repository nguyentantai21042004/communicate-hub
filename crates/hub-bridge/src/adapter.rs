//! Object-safe handle over [`hub_core::NetworkAdapter`].
//!
//! The trait returns `impl Future`, which is not object-safe. The bridge serves more than one
//! network, so it stores [`AdapterHandle`] trait objects. Every [`NetworkAdapter`] implements it.

use std::future::Future;
use std::pin::Pin;

use hub_core::{MessageRef, ModuleHealth, Network, NetworkAdapter, OutgoingMessage, RemoteEvent};

/// Boxed future returned by [`AdapterHandle`].
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// [`NetworkAdapter`] as a trait object.
pub trait AdapterHandle: Send + Sync {
    /// Network this handle speaks.
    fn network(&self) -> Network;

    /// Waits for the next event. `Ok(None)` means the adapter shut down cleanly.
    fn next_event(&self) -> BoxFut<'_, hub_core::Result<Option<RemoteEvent>>>;

    /// Sends one message and returns the id the network assigned.
    fn send(&self, message: OutgoingMessage) -> BoxFut<'_, hub_core::Result<MessageRef>>;

    /// Applies or removes `emoji` on `target`.
    fn react(
        &self,
        target: MessageRef,
        emoji: String,
        added: bool,
    ) -> BoxFut<'_, hub_core::Result<()>>;

    /// Removes a message this account sent.
    fn unsend(&self, target: MessageRef) -> BoxFut<'_, hub_core::Result<()>>;

    /// Reports the adapter's current health.
    fn health(&self) -> BoxFut<'_, hub_core::Result<ModuleHealth>>;
}

impl<A> AdapterHandle for A
where
    A: NetworkAdapter,
{
    fn network(&self) -> Network {
        NetworkAdapter::network(self)
    }

    fn next_event(&self) -> BoxFut<'_, hub_core::Result<Option<RemoteEvent>>> {
        Box::pin(NetworkAdapter::next_event(self))
    }

    fn send(&self, message: OutgoingMessage) -> BoxFut<'_, hub_core::Result<MessageRef>> {
        Box::pin(NetworkAdapter::send(self, message))
    }

    fn react(
        &self,
        target: MessageRef,
        emoji: String,
        added: bool,
    ) -> BoxFut<'_, hub_core::Result<()>> {
        Box::pin(NetworkAdapter::react(self, target, emoji, added))
    }

    fn unsend(&self, target: MessageRef) -> BoxFut<'_, hub_core::Result<()>> {
        Box::pin(NetworkAdapter::unsend(self, target))
    }

    fn health(&self) -> BoxFut<'_, hub_core::Result<ModuleHealth>> {
        Box::pin(NetworkAdapter::health(self))
    }
}
