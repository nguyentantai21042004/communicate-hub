//! Inbound conversion: raw MTProto updates to core [`InboundEvent`] values.
//!
//! The raw TL update is serialized before grammers wraps it, so every event carries the exact
//! payload Telegram sent, including fields this layer does not normalize.

use captain_core::{
    ChatId, ChatKind, ChatSummary, EventKind, InboundEvent, MediaSummary, MessageId, MessageView,
};
use grammers_client::peer::Peer;
use grammers_client::session::types::{PeerId, PeerKind};
use grammers_client::tl;
use grammers_client::update::Update;
use serde::Serialize;

use crate::entities;

const UNKNOWN_VARIANT: &str = "unknown"; // raw_type when a payload is not a tagged TL enum

/// Serialized raw update, taken before [`Update::from_raw`] consumes it.
pub(crate) struct RawPayload {
    pub(crate) raw_type: String,
    pub(crate) raw: serde_json::Value,
}

impl RawPayload {
    pub(crate) fn capture(update: &tl::enums::Update) -> Self {
        let (raw_type, raw) = split_variant(update);
        Self { raw_type, raw }
    }
}

/// Builds the event for one update. `self_id` resolves the self-user placeholder grammers uses
/// when Telegram omits the account's own id.
pub(crate) fn to_event(
    update: Update,
    payload: RawPayload,
    self_id: i64,
    now_unix: i64,
) -> InboundEvent {
    let kind = match update {
        Update::NewMessage(m) => EventKind::MessageNew(message_view(&m, self_id)),
        Update::MessageEdited(m) => EventKind::MessageEdited(message_view(&m, self_id)),
        Update::MessageDeleted(d) => EventKind::MessagesDeleted {
            chat: d
                .channel_id()
                .and_then(PeerId::channel)
                .map(|p| chat_id(p, self_id)),
            ids: d.messages().iter().copied().map(MessageId).collect(),
        },
        _ => EventKind::Other,
    };
    InboundEvent {
        received_at_unix: now_unix,
        kind,
        raw_type: payload.raw_type,
        raw: payload.raw,
    }
}

/// Normalizes one message. Shared with the outbound path, which reports sent messages the same
/// way.
pub(crate) fn message_view(m: &grammers_client::message::Message, self_id: i64) -> MessageView {
    let chat_peer = m.peer_id();
    MessageView {
        chat: summary(chat_peer, m.peer(), self_id),
        id: MessageId(m.id()),
        sender: m.sender_id().map(|id| summary(id, m.sender(), self_id)),
        outgoing: m.outgoing(),
        date_unix: m.date().timestamp(),
        text: entities::from_tl(m.text(), m.fmt_entities()),
        media: raw_media(&m.raw).and_then(media_summary),
        reply_to: m.reply_to_message_id().map(MessageId),
        album_id: m.grouped_id(),
    }
}

/// Maps a grammers peer id to the core marked id.
pub(crate) fn chat_id(peer: PeerId, self_id: i64) -> ChatId {
    ChatId(peer.bot_api_dialog_id().unwrap_or(self_id))
}

fn summary(id: PeerId, peer: Option<&Peer>, self_id: i64) -> ChatSummary {
    ChatSummary {
        id: chat_id(id, self_id),
        kind: match id.kind() {
            PeerKind::User => ChatKind::User,
            PeerKind::Chat => ChatKind::Group,
            PeerKind::Channel => ChatKind::Channel,
        },
        name: peer.and_then(Peer::name).map(str::to_owned),
        username: peer.and_then(Peer::username).map(str::to_owned),
    }
}

fn raw_media(message: &tl::enums::Message) -> Option<&tl::enums::MessageMedia> {
    match message {
        tl::enums::Message::Message(m) => m.media.as_ref(),
        _ => None,
    }
}

fn media_summary(media: &tl::enums::MessageMedia) -> Option<MediaSummary> {
    use tl::enums::MessageMedia as M;

    let raw_type = split_variant(media).0;
    let mut out = MediaSummary {
        raw_type,
        mime_type: None,
        file_name: None,
        size_bytes: None,
        spoiler: false,
    };
    match media {
        M::Empty => return None,
        M::Photo(p) => out.spoiler = p.spoiler,
        M::Document(d) => {
            out.spoiler = d.spoiler;
            if let Some(tl::enums::Document::Document(doc)) = &d.document {
                out.mime_type = Some(doc.mime_type.clone());
                out.size_bytes = u64::try_from(doc.size).ok();
                out.file_name = doc.attributes.iter().find_map(|a| match a {
                    tl::enums::DocumentAttribute::Filename(f) => Some(f.file_name.clone()),
                    _ => None,
                });
            }
        }
        _ => {}
    }
    Some(out)
}

/// Splits a serde-serialized TL enum into its variant name and its fields.
///
/// TELEGRAM: grammers derives serde with the default external tagging, so a value serializes as
/// `{"NewMessage": {...}}`, or as the bare string `"Empty"` for constructors without fields.
fn split_variant(value: &impl Serialize) -> (String, serde_json::Value) {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(map)) if map.len() == 1 => {
            let mut entries = map.into_iter();
            match entries.next() {
                Some((name, fields)) => (name, fields),
                None => (UNKNOWN_VARIANT.to_owned(), serde_json::Value::Null),
            }
        }
        Ok(serde_json::Value::String(name)) => (name, serde_json::Value::Null),
        Ok(other) => (UNKNOWN_VARIANT.to_owned(), other),
        Err(e) => (
            UNKNOWN_VARIANT.to_owned(),
            serde_json::Value::String(e.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_update_keeps_variant_name_and_fields() {
        let update = tl::enums::Update::MessageId(tl::types::UpdateMessageId {
            id: 7,
            random_id: 9,
        });
        let payload = RawPayload::capture(&update);
        assert_eq!(payload.raw_type, "MessageId");
        assert_eq!(payload.raw["random_id"], 9);
    }

    #[test]
    fn document_summary_reads_name_mime_and_spoiler() {
        let doc = tl::types::Document {
            id: 1,
            access_hash: 2,
            file_reference: vec![],
            date: 0,
            mime_type: "application/pdf".into(),
            size: 2048,
            thumbs: None,
            video_thumbs: None,
            dc_id: 2,
            attributes: vec![tl::enums::DocumentAttribute::Filename(
                tl::types::DocumentAttributeFilename {
                    file_name: "báo cáo.pdf".into(),
                },
            )],
        };
        let media = tl::enums::MessageMedia::Document(tl::types::MessageMediaDocument {
            nopremium: false,
            spoiler: true,
            video: false,
            round: false,
            voice: false,
            document: Some(doc.into()),
            alt_documents: None,
            video_cover: None,
            video_timestamp: None,
            ttl_seconds: None,
        });
        let s = media_summary(&media).unwrap();
        assert_eq!(s.raw_type, "Document");
        assert_eq!(s.file_name.as_deref(), Some("báo cáo.pdf"));
        assert_eq!(s.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(s.size_bytes, Some(2048));
        assert!(s.spoiler);
    }

    #[test]
    fn empty_media_is_no_media() {
        assert!(media_summary(&tl::enums::MessageMedia::Empty).is_none());
    }

    #[test]
    fn channel_ids_are_marked() {
        let peer = PeerId::channel(1234).unwrap();
        assert_eq!(chat_id(peer, 1), ChatId(-1_000_000_001_234));
        assert_eq!(chat_id(PeerId::self_user(), 42), ChatId(42));
    }
}
