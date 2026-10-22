//! Long-poll parsing for the bridge supergroup.
//!
//! `hub-bridge` defines its own `TopicIncoming`. The types here are not that enum:
//! a message keeps `file_id`, sender, and album id, and a reaction has no thread.
//! A reaction update has no `message_thread_id`, so [`IncomingReaction::topic`]
//! stays empty and the bridge correlates by message id.

use std::future::Future;

use frankenstein::types::{Message, ReactionType, User};
use frankenstein::updates::{Update, UpdateContent};
use hub_core::{AttachmentKind, TgMessageId, TopicId};

use crate::error::Result;
use crate::limits::GENERAL_TOPIC_ID;

/// Remembers the next `getUpdates` offset.
///
/// The caller owns the store. This crate does not write a file.
pub trait OffsetStore: Send + Sync {
    /// Returns the next offset, or `None` when the caller has never stored one.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be read. The poll loop stops.
    fn load(&self) -> impl Future<Output = Result<Option<i64>>> + Send;

    /// Stores the next offset. The value is the last handled `update_id` plus one.
    ///
    /// # Errors
    ///
    /// Returns an error when the store cannot be written. The poll loop stops.
    fn save(&self, offset: i64) -> impl Future<Output = Result<()>> + Send;
}

/// One forum update the bridge should mirror onto a source network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicIncoming {
    /// A person posted in the bridge supergroup.
    Message(IncomingMessage),
    /// A person added or removed an emoji reaction.
    Reaction(IncomingReaction),
}

/// A post in the bridge supergroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    /// Forum topic. The General topic uses [`GENERAL_TOPIC_ID`] when `Telegram` omits the thread.
    pub topic: TopicId,
    /// Bot API `message_id` inside the supergroup.
    pub message: TgMessageId,
    /// Sender's user id.
    pub sender_id: i64,
    /// `first_name` plus `last_name` when `Telegram` sent a last name.
    pub sender_display_name: String,
    /// `text`, or the caption when the post is media. Empty when there is neither.
    pub text: String,
    /// The one attachment `Telegram` put on the post, most specific kind first.
    pub attachments: Vec<IncomingAttachment>,
    /// The message this post replies to, in the same supergroup.
    pub reply_to: Option<TgMessageId>,
    /// `Telegram` `date`, as Unix seconds.
    pub sent_at_unix_secs: i64,
    /// Album id, when this post is one item of a media group.
    pub media_group_id: Option<String>,
}

/// A file referenced by Bot API `file_id`.
///
/// `hub_core::Attachment` has no file id, so this struct does not reuse it.
/// The bridge downloads or forwards `file_id` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingAttachment {
    /// How the bridge should present the file.
    pub kind: AttachmentKind,
    /// Bot API `file_id` of the chosen size.
    pub file_id: String,
    /// File name `Telegram` sent, or the name this crate fills in for that kind.
    pub file_name: Option<String>,
    /// MIME type `Telegram` sent, or the type implied by a sticker.
    pub mime: Option<String>,
    /// Size in bytes, when `Telegram` sent one.
    pub size_bytes: Option<u64>,
    /// Length in seconds for voice, audio, and video.
    pub duration_secs: Option<u32>,
}

/// An emoji added to or removed from a supergroup post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingReaction {
    /// Forum topic, when the update carries `message_thread_id`.
    ///
    /// TELEGRAM: `message_reaction` has no thread id, so this is `None`.
    pub topic: Option<TopicId>,
    /// Post the reaction belongs to.
    pub message: TgMessageId,
    /// User who changed the reaction.
    pub user_id: i64,
    /// Emoji that was added or removed.
    pub emoji: String,
    /// `true` when the emoji is now present, `false` when it was removed.
    pub added: bool,
}

pub(crate) fn from_update(update: &Update, group_id: i64, bot_id: u64) -> Vec<TopicIncoming> {
    match &update.content {
        UpdateContent::Message(message) => message_incoming(message, group_id, bot_id)
            .into_iter()
            .collect(),
        UpdateContent::MessageReaction(reaction) => {
            if reaction.chat.id != group_id {
                return Vec::new();
            }
            let Some(user) = &reaction.user else {
                return Vec::new();
            };
            if user.id == bot_id {
                return Vec::new();
            }
            let Ok(user_id) = i64::try_from(user.id) else {
                return Vec::new();
            };
            let old = emoji_list(&reaction.old_reaction);
            let new = emoji_list(&reaction.new_reaction);
            let mut events = Vec::new();
            for emoji in &new {
                if !old.contains(emoji) {
                    events.push(TopicIncoming::Reaction(IncomingReaction {
                        topic: None,
                        message: TgMessageId(reaction.message_id),
                        user_id,
                        emoji: emoji.clone(),
                        added: true,
                    }));
                }
            }
            for emoji in &old {
                if !new.contains(emoji) {
                    events.push(TopicIncoming::Reaction(IncomingReaction {
                        topic: None,
                        message: TgMessageId(reaction.message_id),
                        user_id,
                        emoji: emoji.clone(),
                        added: false,
                    }));
                }
            }
            events
        }
        _ => Vec::new(),
    }
}

fn message_incoming(message: &Message, group_id: i64, bot_id: u64) -> Option<TopicIncoming> {
    if message.chat.id != group_id {
        return None;
    }
    let from = message.from.as_deref()?;
    if from.id == bot_id {
        return None;
    }
    let Ok(sender_id) = i64::try_from(from.id) else {
        return None;
    };
    // TELEGRAM: the General topic omits message_thread_id. Its id is 1.
    let topic = TopicId(message.message_thread_id.unwrap_or(GENERAL_TOPIC_ID));
    let text = message
        .text
        .clone()
        .or_else(|| message.caption.clone())
        .unwrap_or_default();
    Some(TopicIncoming::Message(IncomingMessage {
        topic,
        message: TgMessageId(message.message_id),
        sender_id,
        sender_display_name: display_name(from),
        text,
        attachments: detect_attachment(message).into_iter().collect(),
        reply_to: message
            .reply_to_message
            .as_ref()
            .map(|reply| TgMessageId(reply.message_id)),
        sent_at_unix_secs: i64::try_from(message.date).unwrap_or(0),
        media_group_id: message.media_group_id.clone(),
    }))
}

fn display_name(user: &User) -> String {
    match &user.last_name {
        Some(last) if !last.is_empty() => format!("{} {last}", user.first_name),
        _ => user.first_name.clone(),
    }
}

fn emoji_list(reactions: &[ReactionType]) -> Vec<String> {
    reactions
        .iter()
        .filter_map(|reaction| match reaction {
            ReactionType::Emoji(emoji) => Some(emoji.emoji.clone()),
            ReactionType::CustomEmoji(_) | ReactionType::Paid(_) => None,
        })
        .collect()
}

fn detect_attachment(message: &Message) -> Option<IncomingAttachment> {
    if let Some(animation) = &message.animation {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Gif,
            file_id: animation.file_id.clone(),
            file_name: Some(
                animation
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "animation.mp4".to_owned()),
            ),
            mime: animation.mime_type.clone(),
            size_bytes: animation.file_size,
            duration_secs: Some(animation.duration),
        });
    }
    if let Some(photo) = message.photo.as_ref().and_then(|sizes| sizes.last()) {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Photo,
            file_id: photo.file_id.clone(),
            file_name: Some("photo.jpg".to_owned()),
            mime: Some("image/jpeg".to_owned()),
            size_bytes: photo.file_size,
            duration_secs: None,
        });
    }
    if let Some(sticker) = &message.sticker {
        let (file_name, mime) = if sticker.is_video {
            ("sticker.webm", "video/webm")
        } else if sticker.is_animated {
            ("sticker.tgs", "application/x-tgsticker")
        } else {
            ("sticker.webp", "image/webp")
        };
        return Some(IncomingAttachment {
            kind: AttachmentKind::Sticker,
            file_id: sticker.file_id.clone(),
            file_name: Some(file_name.to_owned()),
            mime: Some(mime.to_owned()),
            size_bytes: sticker.file_size,
            duration_secs: None,
        });
    }
    if let Some(voice) = &message.voice {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Voice,
            file_id: voice.file_id.clone(),
            file_name: Some("voice.ogg".to_owned()),
            mime: voice.mime_type.clone(),
            size_bytes: voice.file_size,
            duration_secs: Some(voice.duration),
        });
    }
    if let Some(note) = &message.video_note {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Video,
            file_id: note.file_id.clone(),
            file_name: Some("video-note.mp4".to_owned()),
            mime: Some("video/mp4".to_owned()),
            size_bytes: note.file_size,
            duration_secs: Some(note.duration),
        });
    }
    if let Some(video) = &message.video {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Video,
            file_id: video.file_id.clone(),
            file_name: Some(
                video
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "video.mp4".to_owned()),
            ),
            mime: video.mime_type.clone(),
            size_bytes: video.file_size,
            duration_secs: Some(video.duration),
        });
    }
    if let Some(audio) = &message.audio {
        return Some(IncomingAttachment {
            kind: AttachmentKind::Audio,
            file_id: audio.file_id.clone(),
            file_name: Some(
                audio
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "audio.mp3".to_owned()),
            ),
            mime: audio.mime_type.clone(),
            size_bytes: audio.file_size,
            duration_secs: Some(audio.duration),
        });
    }
    if let Some(document) = &message.document {
        return Some(IncomingAttachment {
            kind: AttachmentKind::File,
            file_id: document.file_id.clone(),
            file_name: Some(
                document
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "file".to_owned()),
            ),
            mime: document.mime_type.clone(),
            size_bytes: document.file_size,
            duration_secs: None,
        });
    }
    None
}
