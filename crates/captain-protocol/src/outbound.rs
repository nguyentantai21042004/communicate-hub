//! Outbound conversion: core [`OutgoingMessage`] values to MTProto send requests.
//!
//! Text and single media go through grammers' `send_message`. Albums are sent with a raw
//! `messages.sendMultiMedia` call, because grammers' `send_album` hardcodes `silent` and
//! `invert_media` to false and would drop the caption-above and silent options.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use captain_core::limits::PHOTO_MAX_BYTES;
use captain_core::{
    ChatRef, Content, FormattedText, MediaItem, MediaKind, MediaSource, MessageId, OutgoingMessage,
    PhotoQuality, SendOptions, SentMessage,
};
use grammers_client::media::Uploaded;
use grammers_client::message::InputMessage;
use grammers_client::session::Session;
use grammers_client::session::types::{PeerId, PeerRef};
use grammers_client::{Client, tl};

use crate::error::rpc;
use crate::inbound::chat_id;
use crate::session::KeychainSession;
use crate::{Error, Result};

const DEFAULT_MIME: &str = "application/octet-stream";

/// Everything a send needs from the live connection.
pub(crate) struct Outbound<'a> {
    pub(crate) client: &'a Client,
    pub(crate) session: &'a KeychainSession,
    pub(crate) self_id: i64,
}

impl Outbound<'_> {
    /// Validates and sends one message or album, returning one entry per sent message.
    pub(crate) async fn send(&self, message: OutgoingMessage) -> Result<Vec<SentMessage>> {
        message.validate()?;
        let peer = self.resolve(&message.to).await?;
        match message.content {
            Content::Text(text) => self.send_text(peer, text, &message.options).await,
            Content::Media {
                items,
                caption,
                caption_above,
            } => {
                if let [item] = items.as_slice() {
                    self.send_single(peer, item, caption, caption_above, &message.options)
                        .await
                } else {
                    self.send_album(peer, &items, caption, caption_above, &message.options)
                        .await
                }
            }
        }
    }

    /// Resolves a chat reference to a peer Telegram accepts.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChat`] when a numeric id is not in the session's peer cache: Telegram
    /// needs the access hash that only comes with a message or a username lookup.
    pub(crate) async fn resolve(&self, chat: &ChatRef) -> Result<PeerRef> {
        match chat {
            ChatRef::Me => {
                let cached = match PeerId::user(self.self_id) {
                    Some(id) => self.cached_ref(id).await?,
                    None => None,
                };
                Ok(cached.unwrap_or_else(|| PeerId::self_user().to_ambient_ref()))
            }
            ChatRef::Username(name) => {
                let peer = self
                    .client
                    .resolve_username(name)
                    .await
                    .map_err(rpc("contacts.resolveUsername"))?
                    .ok_or_else(|| Error::UnknownChat(format!("@{name}")))?;
                peer.to_ref()
                    .await
                    .map_err(|e| Error::Session(e.to_string()))?
                    .ok_or_else(|| Error::UnknownChat(format!("@{name}")))
            }
            ChatRef::Id(id) => {
                let unknown = || Error::UnknownChat(id.0.to_string());
                let peer = PeerId::from_bot_api_dialog_id(id.0).ok_or_else(unknown)?;
                self.cached_ref(peer).await?.ok_or_else(unknown)
            }
        }
    }

    async fn cached_ref(&self, peer: PeerId) -> Result<Option<PeerRef>> {
        self.session
            .peer_ref(peer)
            .await
            .map_err(|e| Error::Session(e.to_string()))
    }

    async fn send_text(
        &self,
        peer: PeerRef,
        text: FormattedText,
        options: &SendOptions,
    ) -> Result<Vec<SentMessage>> {
        let input = base_input(text, options).link_preview(!options.no_link_preview);
        self.send_input(peer, input).await
    }

    async fn send_single(
        &self,
        peer: PeerRef,
        item: &MediaItem,
        caption: FormattedText,
        caption_above: bool,
        options: &SendOptions,
    ) -> Result<Vec<SentMessage>> {
        let media = self.upload(item).await?;
        let input = base_input(caption, options)
            .invert_media(caption_above)
            .media(media);
        self.send_input(peer, input).await
    }

    async fn send_input(&self, peer: PeerRef, input: InputMessage) -> Result<Vec<SentMessage>> {
        let sent = self
            .client
            .send_message(peer, input)
            .await
            .map_err(rpc("messages.sendMessage"))?;
        Ok(vec![SentMessage {
            chat_id: chat_id(sent.peer_id(), self.self_id),
            id: MessageId(sent.id()),
            raw: to_json(&sent.raw),
        }])
    }

    async fn send_album(
        &self,
        peer: PeerRef,
        items: &[MediaItem],
        caption: FormattedText,
        caption_above: bool,
        options: &SendOptions,
    ) -> Result<Vec<SentMessage>> {
        let input_peer = tl::enums::InputPeer::from(peer);
        let mut caption_entities = Some(self.input_entities(&caption).await?);
        let mut caption_text = Some(caption.text);
        let mut random_ids = Vec::with_capacity(items.len());
        let mut multi_media = Vec::with_capacity(items.len());

        for item in items {
            let uploaded = self.upload(item).await?;
            // TELEGRAM: sendMultiMedia only takes media already on the server, so each upload is
            // first turned into a photo or document with messages.uploadMedia.
            let on_server = self
                .client
                .invoke(&tl::functions::messages::UploadMedia {
                    business_connection_id: None,
                    peer: input_peer.clone(),
                    media: uploaded,
                })
                .await
                .map_err(rpc("messages.uploadMedia"))?;
            let random_id = random_id();
            random_ids.push(random_id);
            // TELEGRAM: an album shows a caption only when exactly one item carries it, so it
            // goes on the first item and the rest stay empty.
            multi_media.push(tl::enums::InputSingleMedia::Media(
                tl::types::InputSingleMedia {
                    media: reference_media(on_server, spoiler_of(item))?,
                    random_id,
                    message: caption_text.take().unwrap_or_default(),
                    entities: caption_entities.take().filter(|e| !e.is_empty()),
                },
            ));
        }

        let updates = self
            .client
            .invoke(&tl::functions::messages::SendMultiMedia {
                silent: options.silent,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: caption_above,
                allow_paid_floodskip: false,
                peer: input_peer,
                reply_to: options.reply_to.map(input_reply_to),
                multi_media,
                schedule_date: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
            })
            .await
            .map_err(rpc("messages.sendMultiMedia"))?;
        Ok(sent_from_updates(
            &updates,
            &random_ids,
            chat_id(peer.id, self.self_id),
        ))
    }

    /// Uploads one item and returns the media to attach it with.
    async fn upload(&self, item: &MediaItem) -> Result<tl::enums::InputMedia> {
        let (name, uploaded) = match &item.source {
            MediaSource::Path(path) => {
                let name = file_name(path);
                let file_err = |source| Error::File {
                    path: path.clone(),
                    source,
                };
                let mut file = tokio::fs::File::open(path).await.map_err(file_err)?;
                let size_bytes = file.metadata().await.map_err(file_err)?.len();
                check_photo_size(item, &name, size_bytes)?;
                let size = usize::try_from(size_bytes).unwrap_or(usize::MAX);
                let uploaded = self
                    .client
                    .upload_stream(&mut file, size, name.clone())
                    .await
                    .map_err(file_err)?;
                (name, uploaded)
            }
            MediaSource::Bytes { name, data } => {
                check_photo_size(item, name, data.len() as u64)?;
                let mut reader: &[u8] = data;
                let uploaded = self
                    .client
                    .upload_stream(&mut reader, data.len(), name.clone())
                    .await
                    .map_err(|source| Error::File {
                        path: name.into(),
                        source,
                    })?;
                (name.clone(), uploaded)
            }
        };
        Ok(uploaded_media(item.kind, &name, uploaded))
    }

    /// Converts caption entities for a raw request. `send_message` does this itself; the raw
    /// album call has to.
    async fn input_entities(&self, text: &FormattedText) -> Result<Vec<tl::enums::MessageEntity>> {
        let mut entities = crate::entities::to_tl(&text.entities);
        for entity in &mut entities {
            if let tl::enums::MessageEntity::MentionName(m) = entity {
                // TELEGRAM: the server rejects messageEntityMentionName on input; it needs
                // inputMessageEntityMentionName with the user's access hash.
                let unknown = || Error::UnknownChat(m.user_id.to_string());
                let user = PeerId::user(m.user_id).ok_or_else(unknown)?;
                let peer = self.cached_ref(user).await?.ok_or_else(unknown)?;
                *entity = tl::types::InputMessageEntityMentionName {
                    offset: m.offset,
                    length: m.length,
                    user_id: peer.into(),
                }
                .into();
            }
        }
        Ok(entities)
    }
}

fn base_input(text: FormattedText, options: &SendOptions) -> InputMessage {
    InputMessage::new()
        .fmt_entities(crate::entities::to_tl(&text.entities))
        .text(text.text)
        .silent(options.silent)
        .reply_to(options.reply_to.map(|id| id.0))
}

fn uploaded_media(kind: MediaKind, name: &str, uploaded: Uploaded) -> tl::enums::InputMedia {
    match kind {
        MediaKind::Photo {
            quality: PhotoQuality::Hd,
            spoiler,
        } => tl::types::InputMediaUploadedPhoto {
            spoiler,
            live_photo: false,
            file: uploaded.raw,
            stickers: None,
            ttl_seconds: None,
            video: None,
        }
        .into(),
        MediaKind::Photo {
            quality: PhotoQuality::Original,
            spoiler,
        } => uploaded_document(name, uploaded, spoiler),
        MediaKind::Document => uploaded_document(name, uploaded, false),
    }
}

fn uploaded_document(name: &str, uploaded: Uploaded, spoiler: bool) -> tl::enums::InputMedia {
    let mime_type = mime_guess::from_path(name)
        .first_raw()
        .unwrap_or(DEFAULT_MIME)
        .to_owned();
    tl::types::InputMediaUploadedDocument {
        nosound_video: false,
        // Keeps the server from turning an image or video into inline media: "original" means
        // the recipient gets the exact bytes as a file.
        force_file: true,
        spoiler,
        file: uploaded.raw,
        thumb: None,
        mime_type,
        attributes: vec![
            tl::types::DocumentAttributeFilename {
                file_name: name.to_owned(),
            }
            .into(),
        ],
        stickers: None,
        video_cover: None,
        video_timestamp: None,
        ttl_seconds: None,
    }
    .into()
}

/// Turns the result of `messages.uploadMedia` into a reference `sendMultiMedia` accepts.
fn reference_media(media: tl::enums::MessageMedia, spoiler: bool) -> Result<tl::enums::InputMedia> {
    let unexpected = |detail: &str| Error::Unexpected {
        request: "messages.uploadMedia",
        detail: detail.to_owned(),
    };
    match media {
        tl::enums::MessageMedia::Photo(p) => match p.photo {
            Some(tl::enums::Photo::Photo(photo)) => Ok(tl::types::InputMediaPhoto {
                spoiler,
                live_photo: false,
                id: tl::types::InputPhoto {
                    id: photo.id,
                    access_hash: photo.access_hash,
                    file_reference: photo.file_reference,
                }
                .into(),
                ttl_seconds: None,
                video: None,
            }
            .into()),
            _ => Err(unexpected("photo missing from uploaded media")),
        },
        tl::enums::MessageMedia::Document(d) => match d.document {
            Some(tl::enums::Document::Document(doc)) => Ok(tl::types::InputMediaDocument {
                spoiler,
                id: tl::types::InputDocument {
                    id: doc.id,
                    access_hash: doc.access_hash,
                    file_reference: doc.file_reference,
                }
                .into(),
                video_cover: None,
                video_timestamp: None,
                ttl_seconds: None,
                query: None,
            }
            .into()),
            _ => Err(unexpected("document missing from uploaded media")),
        },
        _ => Err(unexpected("media is neither a photo nor a document")),
    }
}

/// Pairs each random id with the message Telegram created for it, in send order.
fn sent_from_updates(
    updates: &tl::enums::Updates,
    random_ids: &[i64],
    chat: captain_core::ChatId,
) -> Vec<SentMessage> {
    let list: &[tl::enums::Update] = match updates {
        tl::enums::Updates::Updates(u) => &u.updates,
        tl::enums::Updates::Combined(u) => &u.updates,
        _ => &[],
    };
    let id_for = |random_id: i64| {
        list.iter().find_map(|u| match u {
            tl::enums::Update::MessageId(m) if m.random_id == random_id => Some(m.id),
            _ => None,
        })
    };
    let message_for = |id: i32| {
        list.iter().find_map(|u| {
            let message = match u {
                tl::enums::Update::NewMessage(m) => &m.message,
                tl::enums::Update::NewChannelMessage(m) => &m.message,
                _ => return None,
            };
            (message_id(message) == id).then(|| to_json(message))
        })
    };
    random_ids
        .iter()
        .filter_map(|&random_id| id_for(random_id))
        .map(|id| SentMessage {
            chat_id: chat,
            id: MessageId(id),
            raw: message_for(id).unwrap_or(serde_json::Value::Null),
        })
        .collect()
}

fn message_id(message: &tl::enums::Message) -> i32 {
    match message {
        tl::enums::Message::Message(m) => m.id,
        tl::enums::Message::Service(m) => m.id,
        tl::enums::Message::Empty(m) => m.id,
    }
}

fn input_reply_to(id: MessageId) -> tl::enums::InputReplyTo {
    tl::types::InputReplyToMessage {
        reply_to_msg_id: id.0,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text: None,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
    }
    .into()
}

fn check_photo_size(item: &MediaItem, name: &str, size_bytes: u64) -> Result<()> {
    if item.is_photo() && size_bytes > PHOTO_MAX_BYTES {
        return Err(Error::PhotoTooLarge {
            name: name.to_owned(),
            size_bytes,
            max_bytes: PHOTO_MAX_BYTES,
        });
    }
    Ok(())
}

fn spoiler_of(item: &MediaItem) -> bool {
    matches!(item.kind, MediaKind::Photo { spoiler: true, .. })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn to_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Random id for `sendMultiMedia`, which the server uses to deduplicate retries.
///
/// grammers keeps its generator private. `RandomState` is seeded from the OS once per process;
/// hashing a counter and the clock through it gives distinct, unpredictable ids without adding
/// a dependency.
fn random_id() -> i64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(nanos);
    i64::from_ne_bytes(hasher.finish().to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_ids_do_not_repeat() {
        let ids: std::collections::HashSet<i64> = (0..1000).map(|_| random_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn album_results_follow_send_order() {
        let update = |id, random_id| {
            tl::enums::Update::MessageId(tl::types::UpdateMessageId { id, random_id })
        };
        let updates = tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![update(11, 200), update(10, 100)],
            users: vec![],
            chats: vec![],
            date: 0,
            seq: 0,
        });
        let sent = sent_from_updates(&updates, &[100, 200], captain_core::ChatId(5));
        let ids: Vec<i32> = sent.iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![10, 11]);
    }

    #[test]
    fn hd_photo_over_limit_is_rejected_but_original_is_not() {
        let hd = MediaItem::photo("big.jpg");
        assert!(matches!(
            check_photo_size(&hd, "big.jpg", PHOTO_MAX_BYTES + 1),
            Err(Error::PhotoTooLarge { .. })
        ));
        let mut original = hd;
        original.kind = MediaKind::Photo {
            quality: PhotoQuality::Original,
            spoiler: false,
        };
        check_photo_size(&original, "big.jpg", PHOTO_MAX_BYTES + 1).unwrap();
    }
}
