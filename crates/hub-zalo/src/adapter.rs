//! [`hub_core::NetworkAdapter`] for one Zalo account.
//!
//! Outgoing calls become sidecar methods. Sidecar events become [`hub_core::RemoteEvent`].
//! A `Url` attachment is rejected: the sidecar uploads a local file.

use std::sync::Arc;

use hub_core::{
    AttachmentKind, AttachmentSource, MessageRef, Network, NetworkAdapter, OutgoingMessage,
    RemoteEvent,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::ZaloAdmin;
use crate::error::Error;
use crate::normalize::remote_event;
use crate::protocol::{SidecarEvent, map_rpc, message_id_of};
use crate::sidecar::{RunPhase, Sidecar, SidecarConfig};

struct Inbox {
    events: broadcast::Receiver<SidecarEvent>,
    phase: watch::Receiver<RunPhase>,
}

/// Source adapter for Zalo. Clone is not implemented; share it behind `Arc` at the call site.
pub struct ZaloAdapter {
    sidecar: Arc<Sidecar>,
    inbox: tokio::sync::Mutex<Inbox>,
}

impl ZaloAdapter {
    /// Subscribes to the sidecar, then starts the process.
    ///
    /// The admin handle is returned beside the adapter so login events are not missed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] when the supervisor is already running.
    pub fn start(config: SidecarConfig) -> crate::error::Result<(Self, ZaloAdmin)> {
        let sidecar = Sidecar::new(config)?;
        let adapter = Self::from_sidecar(&sidecar);
        let admin = ZaloAdmin::from_sidecar(&sidecar);
        sidecar.launch()?;
        Ok((adapter, admin))
    }

    pub(crate) fn from_sidecar(sidecar: &Arc<Sidecar>) -> Self {
        Self {
            sidecar: sidecar.clone(),
            inbox: tokio::sync::Mutex::new(Inbox {
                events: sidecar.subscribe(),
                phase: sidecar.watch_phase(),
            }),
        }
    }

    /// The process supervisor. Dashboard code uses [`ZaloAdmin`] for login and directory calls.
    pub fn sidecar(&self) -> &Arc<Sidecar> {
        &self.sidecar
    }

    async fn next_remote(&self) -> hub_core::Result<Option<RemoteEvent>> {
        let mut inbox = self.inbox.lock().await;
        let Inbox { events, phase } = &mut *inbox;
        loop {
            match events.try_recv() {
                Ok(event) => {
                    if let Some(remote) = remote_event(&event) {
                        return Ok(Some(remote));
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => return idle(*phase.borrow()),
                Err(broadcast::error::TryRecvError::Empty) => {
                    if let Some(done) = terminal(*phase.borrow()) {
                        return done;
                    }
                    tokio::select! {
                        event = events.recv() => {
                            match event {
                                Ok(event) => {
                                    if let Some(remote) = remote_event(&event) {
                                        return Ok(Some(remote));
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => {
                                    return idle(*phase.borrow());
                                }
                            }
                        }
                        changed = phase.changed() => {
                            if changed.is_err() {
                                return idle(*phase.borrow());
                            }
                        }
                    }
                }
            }
        }
    }
}

impl NetworkAdapter for ZaloAdapter {
    fn network(&self) -> Network {
        Network::Zalo
    }

    async fn next_event(&self) -> hub_core::Result<Option<RemoteEvent>> {
        self.next_remote().await
    }

    async fn send(&self, message: OutgoingMessage) -> hub_core::Result<MessageRef> {
        let conversation = message.conversation.to_string();
        if message.attachments.is_empty() {
            if message.text.trim().is_empty() {
                return Err(hub_core::Error::Adapter {
                    detail: "unsupported".into(),
                });
            }
            let mut params = json!({ "conversation": conversation, "text": message.text });
            if let Some(reply) = &message.reply_to {
                params["reply_to"] = json!({ "message_id": reply.message_id });
            }
            let result = self
                .sidecar
                .call("send.text", params)
                .await
                .map_err(map_rpc)?;
            return Ok(MessageRef {
                conversation: message.conversation,
                message_id: message_id_of(&result).map_err(map_rpc)?,
            });
        }
        let mut last_id = String::new();
        for (index, attachment) in message.attachments.iter().enumerate() {
            let AttachmentSource::LocalPath(path) = &attachment.source else {
                return Err(hub_core::Error::Adapter {
                    detail: "unsupported".into(),
                });
            };
            let mut params = json!({ "conversation": conversation, "path": path });
            if index == 0 && !message.text.is_empty() {
                params["caption"] = Value::String(message.text.clone());
            }
            if attachment.kind == AttachmentKind::Voice {
                params["kind"] = json!("voice");
            }
            let result = self
                .sidecar
                .call("send.media", params)
                .await
                .map_err(map_rpc)?;
            last_id = message_id_of(&result).map_err(map_rpc)?;
        }
        Ok(MessageRef {
            conversation: message.conversation,
            message_id: last_id,
        })
    }

    async fn react(&self, target: MessageRef, emoji: String, added: bool) -> hub_core::Result<()> {
        let emoji = if added {
            Value::String(emoji)
        } else {
            Value::Null
        };
        self.sidecar
            .call(
                "send.reaction",
                json!({
                    "conversation": target.conversation.to_string(),
                    "message_id": target.message_id,
                    "emoji": emoji,
                }),
            )
            .await
            .map_err(map_rpc)?;
        Ok(())
    }

    async fn unsend(&self, target: MessageRef) -> hub_core::Result<()> {
        self.sidecar
            .call(
                "send.unsend",
                json!({
                    "conversation": target.conversation.to_string(),
                    "message_id": target.message_id,
                }),
            )
            .await
            .map_err(map_rpc)?;
        Ok(())
    }

    async fn health(&self) -> hub_core::Result<hub_core::ModuleHealth> {
        Ok(self.sidecar.health())
    }
}

fn terminal(phase: RunPhase) -> Option<hub_core::Result<Option<RemoteEvent>>> {
    match phase {
        RunPhase::CrashLoop => Some(Err(map_rpc(Error::CrashLoop))),
        RunPhase::Stopped => Some(Ok(None)),
        RunPhase::Starting | RunPhase::Running | RunPhase::BackingOff => None,
    }
}

fn idle(phase: RunPhase) -> hub_core::Result<Option<RemoteEvent>> {
    terminal(phase).unwrap_or(Ok(None))
}
