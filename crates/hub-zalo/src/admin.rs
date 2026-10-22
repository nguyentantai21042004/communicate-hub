//! Login, directory, and friend calls that the dashboard makes outside [`hub_core::NetworkAdapter`].

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::error::{Error, Result};
use crate::protocol::{SidecarEvent, string_field};
use crate::sidecar::{RunPhase, Sidecar};

/// One person or group returned by the directory methods.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DirectoryEntry {
    /// Zalo user or group id.
    pub id: String,
    /// Display name. A friend alias wins over the Zalo name.
    pub name: String,
    /// Avatar URL already present on the directory list. Absent when Zalo sent none.
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// One pending friend request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FriendRequestEntry {
    /// Requester id.
    pub id: String,
    /// Requester name.
    pub name: String,
    /// Greeting, when Zalo sent one.
    #[serde(default)]
    pub message: Option<String>,
}

/// One `login.qr.state` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrState {
    /// `waiting`, `scanned`, `expired`, or `done`.
    pub state: String,
    /// Present when the phone has scanned the code.
    pub display_name: Option<String>,
    /// Present when a new `PNG` is ready. The data-URL prefix is already removed.
    pub png_base64: Option<String>,
}

struct Inbox {
    events: broadcast::Receiver<SidecarEvent>,
    phase: watch::Receiver<RunPhase>,
}

/// Directory and QR login calls for one Zalo sidecar.
pub struct ZaloAdmin {
    sidecar: Arc<Sidecar>,
    inbox: tokio::sync::Mutex<Inbox>,
}

impl ZaloAdmin {
    pub(crate) fn from_sidecar(sidecar: &Arc<Sidecar>) -> Self {
        Self {
            sidecar: sidecar.clone(),
            inbox: tokio::sync::Mutex::new(Inbox {
                events: sidecar.subscribe(),
                phase: sidecar.watch_phase(),
            }),
        }
    }

    /// Starts a QR login and returns the first `PNG`, base64-encoded.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the login does not produce an image in time.
    pub async fn start_qr(&self) -> Result<String> {
        let result = self.sidecar.call("login.qr.start", json!({})).await?;
        string_field(&result, "png_base64").ok_or(Error::Protocol)
    }

    /// Stops an in-flight QR login. Safe when none is running.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the process is down.
    pub async fn cancel_qr(&self) -> Result<()> {
        self.sidecar.call("login.qr.cancel", json!({})).await?;
        Ok(())
    }

    /// Friends of the logged-in account.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the payload is not the directory list.
    pub async fn friends(&self) -> Result<Vec<DirectoryEntry>> {
        self.entries("directory.friends", json!({}), "friends")
            .await
    }

    /// Groups the account belongs to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the payload is not the directory list.
    pub async fn groups(&self) -> Result<Vec<DirectoryEntry>> {
        self.entries("directory.groups", json!({}), "groups").await
    }

    /// Members of one group. Names may come from the five-minute cache inside the sidecar.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the payload is not the member list.
    pub async fn group_members(&self, group_id: &str) -> Result<Vec<DirectoryEntry>> {
        self.entries(
            "directory.group_members",
            json!({ "group_id": group_id }),
            "members",
        )
        .await
    }

    /// Incoming friend requests.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] when the payload is not the request list.
    pub async fn friend_requests(&self) -> Result<Vec<FriendRequestEntry>> {
        let result = self.sidecar.call("friend.requests", json!({})).await?;
        serde_json::from_value(result.get("requests").cloned().unwrap_or(Value::Null))
            .map_err(|_| Error::Protocol)
    }

    /// Accepts a friend request.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when Zalo rejects the call.
    pub async fn accept_friend(&self, user_id: &str) -> Result<()> {
        self.sidecar
            .call("friend.accept", json!({ "user_id": user_id }))
            .await?;
        Ok(())
    }

    /// Sends a friend request to a user id.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the user is missing or Zalo rejects the call.
    pub async fn send_friend_request(&self, user_id: &str, message: &str) -> Result<()> {
        self.sidecar
            .call(
                "friend.send_request",
                json!({ "user_id": user_id, "message": message }),
            )
            .await?;
        Ok(())
    }

    /// Sends a friend request to a phone number.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the phone does not match a user.
    pub async fn send_friend_request_phone(&self, phone: &str, message: &str) -> Result<()> {
        self.sidecar
            .call(
                "friend.send_request",
                json!({ "phone": phone, "message": message }),
            )
            .await?;
        Ok(())
    }

    /// Joins a group by invite link.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the link is rejected.
    pub async fn join_group(&self, link: &str) -> Result<()> {
        self.sidecar
            .call("group.join", json!({ "link": link }))
            .await?;
        Ok(())
    }

    /// Leaves a group.
    ///
    /// # Errors
    ///
    /// Returns the sidecar error when the group is missing.
    pub async fn leave_group(&self, group_id: &str) -> Result<()> {
        self.sidecar
            .call("group.leave", json!({ "group_id": group_id }))
            .await?;
        Ok(())
    }

    /// Next QR state event. Other sidecar events are skipped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CrashLoop`] or [`Error::Stopped`] when the process will not emit more.
    pub async fn next_qr_state(&self) -> Result<QrState> {
        let mut inbox = self.inbox.lock().await;
        let Inbox { events, phase } = &mut *inbox;
        loop {
            let event =
                next_interesting(events, phase, |event| event.name == "login.qr.state").await?;
            if let Some(state) = qr_state(&event) {
                return Ok(state);
            }
        }
    }

    async fn entries(&self, method: &str, params: Value, key: &str) -> Result<Vec<DirectoryEntry>> {
        let result = self.sidecar.call(method, params).await?;
        serde_json::from_value(result.get(key).cloned().unwrap_or(Value::Null))
            .map_err(|_| Error::Protocol)
    }
}

fn qr_state(event: &SidecarEvent) -> Option<QrState> {
    Some(QrState {
        state: string_field(&event.data, "state")?,
        display_name: string_field(&event.data, "display_name"),
        png_base64: string_field(&event.data, "png_base64"),
    })
}

async fn next_interesting(
    events: &mut broadcast::Receiver<SidecarEvent>,
    phase: &mut watch::Receiver<RunPhase>,
    want: impl Fn(&SidecarEvent) -> bool,
) -> Result<SidecarEvent> {
    loop {
        match events.try_recv() {
            Ok(event) if want(&event) => return Ok(event),
            Ok(_) => continue,
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Closed) => {
                return Err(phase_error(*phase.borrow()));
            }
            Err(broadcast::error::TryRecvError::Empty) => {
                if let Some(err) = terminal(*phase.borrow()) {
                    return Err(err);
                }
                tokio::select! {
                    event = events.recv() => {
                        match event {
                            Ok(event) if want(&event) => return Ok(event),
                            Ok(_) => continue,
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => {
                                return Err(phase_error(*phase.borrow()));
                            }
                        }
                    }
                    changed = phase.changed() => {
                        if changed.is_err() {
                            return Err(phase_error(*phase.borrow()));
                        }
                    }
                }
            }
        }
    }
}

fn terminal(phase: RunPhase) -> Option<Error> {
    match phase {
        RunPhase::CrashLoop => Some(Error::CrashLoop),
        RunPhase::Stopped => Some(Error::Stopped),
        RunPhase::Starting | RunPhase::Running | RunPhase::BackingOff => None,
    }
}

fn phase_error(phase: RunPhase) -> Error {
    terminal(phase).unwrap_or(Error::Stopped)
}
