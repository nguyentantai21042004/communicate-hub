//! Turns sidecar events into [`hub_core::RemoteEvent`].
//!
//! Listener state and `login.qr.state` are not remote events. A `session` value of
//! `disconnected` is health-only: [`hub_core::SessionState`] has no disconnected variant,
//! and this module does not invent `logged_out` or `expired` for a dropped socket.

use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, ConversationKind, DirectoryTitle,
    Mention, MessageRef, Network, RemoteEvent, RemoteMessage, SessionState,
};
use serde_json::Value;

use crate::protocol::{
    SESSION_DISCONNECTED, SESSION_EXPIRED, SESSION_RECONNECTING, SESSION_VALID, SidecarEvent,
    string_field,
};

const RENAMED_TO_PREFIX: &str = "renamed to ";

/// Every remote event one sidecar frame produces.
///
/// A message or a group rename can carry a title or an avatar ahead of the primary event.
/// [`remote_event`] stays the primary event so a caller that wants one value is unchanged.
pub fn remote_events(event: &SidecarEvent) -> Vec<RemoteEvent> {
    match event.name.as_str() {
        "message" => message_events(&event.data),
        "group_event" => group_events(&event.data),
        _ => remote_event(event).into_iter().collect(),
    }
}

/// A sidecar event the adapter should surface, or `None` when the event is health-only.
pub fn remote_event(event: &SidecarEvent) -> Option<RemoteEvent> {
    match event.name.as_str() {
        "message" => message_event(&event.data).map(RemoteEvent::Message),
        "reaction" => reaction_event(&event.data),
        "undo" => undo_event(&event.data),
        "group_event" => group_event(&event.data),
        "friend_request" => friend_event(&event.data),
        "session" => session_event(&event.data),
        "directory" => directory_event(&event.data),
        _ => None,
    }
}

fn message_events(data: &Value) -> Vec<RemoteEvent> {
    let Some(message) = message_event(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let title = string_field(data, "conversation_title").filter(|title| !title.trim().is_empty());
    let avatar = string_field(data, "avatar_url")
        .filter(|url| !url.is_empty())
        .map(AttachmentSource::Url);
    if title.is_some() || avatar.is_some() {
        out.push(RemoteEvent::ChatInfo {
            conversation: message.conversation.clone(),
            title,
            avatar,
        });
    }
    out.push(RemoteEvent::Message(message));
    out
}

fn group_events(data: &Value) -> Vec<RemoteEvent> {
    let Some(event) = group_event(data) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let RemoteEvent::GroupEvent {
        conversation, text, ..
    } = &event
        && let Some(title) = text.strip_prefix(RENAMED_TO_PREFIX)
    {
        let title = title.trim();
        if !title.is_empty() {
            out.push(RemoteEvent::ChatInfo {
                conversation: conversation.clone(),
                title: Some(title.to_owned()),
                avatar: None,
            });
        }
    }
    out.push(event);
    out
}

fn message_event(data: &Value) -> Option<RemoteMessage> {
    let conversation = conversation(data)?;
    let message_id = string_field(data, "message_id")?;
    let reply_to = data.get("reply_to").and_then(|reply| {
        string_field(reply, "message_id").map(|message_id| MessageRef {
            conversation: conversation.clone(),
            message_id,
        })
    });
    Some(RemoteMessage {
        conversation,
        message_id,
        sender_id: string_field(data, "sender_id").unwrap_or_default(),
        sender_display_name: string_field(data, "sender_display_name").unwrap_or_default(),
        text: string_field(data, "text").unwrap_or_default(),
        attachments: attachments(data.get("attachments")),
        reply_to,
        sent_at_unix_secs: i64_field(data, "sent_at_unix_secs").unwrap_or(0),
        mentions: mentions(data.get("mentions")),
    })
}

fn reaction_event(data: &Value) -> Option<RemoteEvent> {
    Some(RemoteEvent::Reaction {
        target: message_ref(data)?,
        emoji: string_field(data, "emoji").unwrap_or_default(),
        added: data.get("added").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn undo_event(data: &Value) -> Option<RemoteEvent> {
    Some(RemoteEvent::Unsend {
        target: message_ref(data)?,
    })
}

fn group_event(data: &Value) -> Option<RemoteEvent> {
    Some(RemoteEvent::GroupEvent {
        conversation: conversation(data)?,
        text: string_field(data, "text").unwrap_or_default(),
        sent_at_unix_secs: i64_field(data, "sent_at_unix_secs").unwrap_or(0),
    })
}

fn friend_event(data: &Value) -> Option<RemoteEvent> {
    Some(RemoteEvent::FriendRequest {
        sender_id: string_field(data, "sender_id")?,
        sender_display_name: string_field(data, "sender_display_name").unwrap_or_default(),
        message: string_field(data, "message").unwrap_or_default(),
    })
}

fn directory_event(data: &Value) -> Option<RemoteEvent> {
    let items = data.get("entries")?.as_array()?;
    let mut entries = Vec::new();
    for item in items {
        let Some(row) = directory_row(item) else {
            continue;
        };
        entries.push(row);
    }
    Some(RemoteEvent::DirectorySnapshot { entries })
}

fn directory_row(item: &Value) -> Option<DirectoryTitle> {
    let kind = match string_field(item, "kind")?.as_str() {
        "dm" => ConversationKind::Dm,
        "group" => ConversationKind::Group,
        _ => return None,
    };
    let external_id = string_field(item, "id")?;
    if external_id.is_empty() {
        return None;
    }
    let title = string_field(item, "name")?;
    let title = title.trim();
    if title.is_empty() || title == external_id {
        return None;
    }
    Some(DirectoryTitle {
        conversation: ConversationId {
            network: Network::Zalo,
            kind,
            external_id,
        },
        title: title.to_owned(),
    })
}

fn session_event(data: &Value) -> Option<RemoteEvent> {
    let state = string_field(data, "state")?;
    let mapped = match state.as_str() {
        SESSION_VALID => SessionState::Valid,
        SESSION_EXPIRED => SessionState::Expired,
        SESSION_RECONNECTING => SessionState::Connecting,
        // A dropped socket is recorded on ModuleHealth by the supervisor.
        SESSION_DISCONNECTED => return None,
        _ => return None,
    };
    Some(RemoteEvent::SessionChanged { state: mapped })
}

fn message_ref(data: &Value) -> Option<MessageRef> {
    Some(MessageRef {
        conversation: conversation(data)?,
        message_id: string_field(data, "message_id")?,
    })
}

fn conversation(data: &Value) -> Option<ConversationId> {
    let raw = string_field(data, "conversation")?;
    raw.parse().ok()
}

fn attachments(value: Option<&Value>) -> Vec<Attachment> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items.iter().filter_map(attachment).collect()
}

fn attachment(value: &Value) -> Option<Attachment> {
    let kind = kind_of(value.get("kind").and_then(Value::as_str)?)?;
    let source = if let Some(path) = string_field(value, "local_path") {
        AttachmentSource::LocalPath(path)
    } else if let Some(url) = string_field(value, "url") {
        AttachmentSource::Url(url)
    } else {
        return None;
    };
    Some(Attachment {
        kind,
        mime: string_field(value, "mime"),
        size_bytes: value.get("size_bytes").and_then(Value::as_u64),
        source,
        file_name: string_field(value, "file_name"),
    })
}

fn mentions(value: Option<&Value>) -> Vec<Mention> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            Some(Mention {
                user_id: string_field(item, "user_id")?,
                display_name: string_field(item, "display_name").unwrap_or_default(),
            })
        })
        .collect()
}

fn kind_of(name: &str) -> Option<AttachmentKind> {
    Some(match name {
        "photo" => AttachmentKind::Photo,
        "video" => AttachmentKind::Video,
        "voice" => AttachmentKind::Voice,
        "audio" => AttachmentKind::Audio,
        "file" => AttachmentKind::File,
        "sticker" => AttachmentKind::Sticker,
        "gif" => AttachmentKind::Gif,
        "location" => AttachmentKind::Location,
        "poll" => AttachmentKind::Poll,
        "bank_card" => AttachmentKind::BankCard,
        "contact" => AttachmentKind::Contact,
        _ => return None,
    })
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::{ConversationKind, SessionState};

    #[test]
    fn disconnected_session_is_not_a_remote_event() {
        let event = SidecarEvent {
            name: "session".into(),
            data: serde_json::json!({"state": "disconnected", "detail": "close_1006"}),
        };
        assert!(remote_event(&event).is_none());
        let valid = SidecarEvent {
            name: "session".into(),
            data: serde_json::json!({"state": "valid"}),
        };
        assert_eq!(
            remote_event(&valid),
            Some(RemoteEvent::SessionChanged {
                state: SessionState::Valid
            })
        );
    }

    #[test]
    fn a_titled_message_emits_chat_info_before_the_message() {
        let event = SidecarEvent {
            name: "message".into(),
            data: serde_json::json!({
                "conversation": "zalo:group:g1",
                "message_id": "m1",
                "sender_id": "u1",
                "text": "hi",
                "conversation_title": "Team",
                "avatar_url": "https://cdn.example/g1.jpg"
            }),
        };
        let events = remote_events(&event);
        assert!(matches!(
            &events[0],
            RemoteEvent::ChatInfo { title, avatar, .. }
                if title.as_deref() == Some("Team")
                    && matches!(avatar, Some(AttachmentSource::Url(url)) if url == "https://cdn.example/g1.jpg")
        ));
        assert!(matches!(events[1], RemoteEvent::Message(_)));
        assert!(matches!(
            remote_event(&event),
            Some(RemoteEvent::Message(_))
        ));
    }

    #[test]
    fn a_group_rename_emits_chat_info_before_the_notice() {
        let event = SidecarEvent {
            name: "group_event".into(),
            data: serde_json::json!({
                "conversation": "zalo:group:g1",
                "text": "renamed to Team",
                "sent_at_unix_secs": 10
            }),
        };
        let events = remote_events(&event);
        assert!(matches!(
            &events[0],
            RemoteEvent::ChatInfo { title: Some(title), .. } if title == "Team"
        ));
        assert!(matches!(events[1], RemoteEvent::GroupEvent { .. }));
    }

    #[test]
    fn a_directory_snapshot_keeps_real_names_only() {
        let event = SidecarEvent {
            name: "directory".into(),
            data: serde_json::json!({
                "entries": [
                    {"kind": "dm", "id": "1001", "name": "An"},
                    {"kind": "group", "id": "1002", "name": "  "},
                    {"kind": "group", "id": "1003", "name": "1003"},
                    {"kind": "channel", "id": "1004", "name": "Nope"},
                    {"kind": "group", "id": "1005", "name": "Binh"}
                ]
            }),
        };
        assert!(matches!(
            remote_event(&event),
            Some(RemoteEvent::DirectorySnapshot { entries })
                if entries.len() == 2
                    && entries[0].conversation.kind == ConversationKind::Dm
                    && entries[0].conversation.external_id == "1001"
                    && entries[0].title == "An"
                    && entries[1].title == "Binh"
        ));
        let missing = SidecarEvent {
            name: "directory".into(),
            data: serde_json::json!({}),
        };
        assert!(remote_event(&missing).is_none());
    }
}
