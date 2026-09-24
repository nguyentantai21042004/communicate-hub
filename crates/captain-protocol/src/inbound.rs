//! Inbound conversion: raw MTProto updates to core [`InboundEvent`] values.
//!
//! The raw TL update is serialized before grammers wraps it, so every event carries the exact
//! payload Telegram sent, including fields this layer does not normalize.

use captain_core::{
    ChatId, ChatKind, ChatSummary, EventKind, ForwardOrigin, InboundEvent, MediaSummary, MessageId,
    MessageView,
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
        Update::Raw(r) => read_inbox(&r.raw, self_id).unwrap_or(EventKind::Other),
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
    let reply = m.reply_header().and_then(|h| match h {
        tl::enums::MessageReplyHeader::Header(h) => Some(h),
        tl::enums::MessageReplyHeader::MessageReplyStoryHeader(_) => None,
    });
    let place = reply.as_ref().map(reply_place).unwrap_or_default();
    MessageView {
        chat: summary(chat_peer, m.peer(), self_id),
        id: MessageId(m.id()),
        sender: m.sender_id().map(|id| summary(id, m.sender(), self_id)),
        outgoing: m.outgoing(),
        date_unix: m.date().timestamp(),
        text: entities::from_tl(m.text(), m.fmt_entities()),
        media: raw_media(&m.raw).and_then(media_summary),
        reply_to: place.reply_to,
        album_id: m.grouped_id(),
        mentioned: m.mentioned(),
        topic: place.topic,
        quote_text: reply.and_then(|h| h.quote_text),
        forward: m.forward_header().map(|h| forward_origin(&h, self_id)),
        edited_at_unix: m.edit_date().map(|d| d.timestamp()),
    }
}

/// Where a message sits relative to other messages: the message it replies to, and its topic.
#[derive(Debug, Default, PartialEq, Eq)]
struct ReplyPlace {
    reply_to: Option<MessageId>,
    topic: Option<MessageId>,
}

/// TELEGRAM: in a forum, `forum_topic` is set on every message inside a topic. A plain message
/// in topic T points `reply_to_msg_id` at T; a reply inside T points `reply_to_msg_id` at the
/// replied message and `reply_to_top_id` at T. See <https://core.telegram.org/api/forum>.
fn reply_place(h: &tl::types::MessageReplyHeader) -> ReplyPlace {
    let msg = h.reply_to_msg_id.map(MessageId);
    if !h.forum_topic {
        return ReplyPlace {
            reply_to: msg,
            topic: None,
        };
    }
    match h.reply_to_top_id {
        Some(top) => ReplyPlace {
            reply_to: msg,
            topic: Some(MessageId(top)),
        },
        None => ReplyPlace {
            reply_to: None,
            topic: msg,
        },
    }
}

fn forward_origin(h: &tl::enums::MessageFwdHeader, self_id: i64) -> ForwardOrigin {
    let tl::enums::MessageFwdHeader::Header(h) = h;
    let marked = |p: &tl::enums::Peer| chat_id(PeerId::from(p.clone()), self_id);
    ForwardOrigin {
        from: h.from_id.as_ref().map(marked),
        date_unix: i64::from(h.date),
        saved_from_chat: h.saved_from_peer.as_ref().map(marked),
        saved_from_message: h.saved_from_msg_id.map(MessageId),
    }
}

/// Read-inbox updates, which grammers only passes through as raw updates.
fn read_inbox(update: &tl::enums::Update, self_id: i64) -> Option<EventKind> {
    use tl::enums::Update as U;

    let channel = |id: i64| PeerId::channel(id).map(|p| chat_id(p, self_id));
    match update {
        U::ReadHistoryInbox(u) => Some(EventKind::ReadInbox {
            chat: chat_id(PeerId::from(u.peer.clone()), self_id),
            topic: u.top_msg_id.map(MessageId),
            max_id: MessageId(u.max_id),
            still_unread: Some(u.still_unread_count),
        }),
        U::ReadChannelInbox(u) => Some(EventKind::ReadInbox {
            chat: channel(u.channel_id)?,
            topic: None,
            max_id: MessageId(u.max_id),
            still_unread: Some(u.still_unread_count),
        }),
        U::ReadChannelDiscussionInbox(u) => Some(EventKind::ReadInbox {
            chat: channel(u.channel_id)?,
            topic: Some(MessageId(u.top_msg_id)),
            max_id: MessageId(u.read_max_id),
            still_unread: None,
        }),
        _ => None,
    }
}

/// Maps a grammers peer id to the core marked id.
pub(crate) fn chat_id(peer: PeerId, self_id: i64) -> ChatId {
    ChatId(peer.bot_api_dialog_id().unwrap_or(self_id))
}

pub(crate) fn summary(id: PeerId, peer: Option<&Peer>, self_id: i64) -> ChatSummary {
    ChatSummary {
        id: chat_id(id, self_id),
        kind: match id.kind() {
            PeerKind::User => ChatKind::User,
            PeerKind::Chat => ChatKind::Group,
            PeerKind::Channel => ChatKind::Channel,
        },
        name: peer.and_then(Peer::name).map(str::to_owned),
        username: peer.and_then(Peer::username).map(str::to_owned),
        is_bot: matches!(peer, Some(Peer::User(u)) if u.is_bot()),
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

    fn header(forum: bool, msg: Option<i32>, top: Option<i32>) -> tl::types::MessageReplyHeader {
        tl::types::MessageReplyHeader {
            reply_to_scheduled: false,
            forum_topic: forum,
            quote: false,
            reply_to_ephemeral: false,
            reply_to_msg_id: msg,
            reply_to_peer_id: None,
            reply_from: None,
            reply_media: None,
            reply_to_top_id: top,
            quote_text: None,
            quote_entities: None,
            quote_offset: None,
            todo_item_id: None,
            poll_option: None,
        }
    }

    #[test]
    fn topic_messages_are_not_replies_to_the_topic_root() {
        let plain = reply_place(&header(true, Some(40), None));
        assert_eq!(plain.reply_to, None);
        assert_eq!(plain.topic, Some(MessageId(40)));

        let reply = reply_place(&header(true, Some(55), Some(40)));
        assert_eq!(reply.reply_to, Some(MessageId(55)));
        assert_eq!(reply.topic, Some(MessageId(40)));

        let normal = reply_place(&header(false, Some(7), None));
        assert_eq!(normal.reply_to, Some(MessageId(7)));
        assert_eq!(normal.topic, None);
    }

    #[test]
    fn read_updates_become_read_inbox_events() {
        let update = tl::enums::Update::ReadChannelInbox(tl::types::UpdateReadChannelInbox {
            folder_id: None,
            channel_id: 1234,
            max_id: 90,
            still_unread_count: 2,
            pts: 1,
        });
        assert_eq!(
            read_inbox(&update, 1),
            Some(EventKind::ReadInbox {
                chat: ChatId(-1_000_000_001_234),
                topic: None,
                max_id: MessageId(90),
                still_unread: Some(2),
            })
        );
    }

    #[test]
    fn channel_ids_are_marked() {
        let peer = PeerId::channel(1234).unwrap();
        assert_eq!(chat_id(peer, 1), ChatId(-1_000_000_001_234));
        assert_eq!(chat_id(PeerId::self_user(), 42), ChatId(42));
    }
}
