//! Turns sidecar events into [`hub_core::RemoteEvent`].
//!
//! Listener state and `login.qr.state` are not remote events. A `session` value of
//! `disconnected` is health-only: [`hub_core::SessionState`] has no disconnected variant,
//! and this module does not invent `logged_out` or `expired` for a dropped socket.

use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, Mention, MessageRef, RemoteEvent,
    RemoteMessage, SessionState,
};
use serde_json::Value;

use crate::protocol::{
    SESSION_DISCONNECTED, SESSION_EXPIRED, SESSION_RECONNECTING, SESSION_VALID, SidecarEvent,
    string_field,
};

/// A sidecar event the adapter should surface, or `None` when the event is health-only.
pub fn remote_event(event: &SidecarEvent) -> Option<RemoteEvent> {
    match event.name.as_str() {
        "message" => message_event(&event.data).map(RemoteEvent::Message),
        "reaction" => reaction_event(&event.data),
        "undo" => undo_event(&event.data),
        "group_event" => group_event(&event.data),
        "friend_request" => friend_event(&event.data),
        "session" => session_event(&event.data),
        _ => None,
    }
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
    use hub_core::SessionState;

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
}
