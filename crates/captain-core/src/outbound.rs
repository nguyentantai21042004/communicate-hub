//! What the account sends: text, media with a caption, or an album, plus the send options
//! Telegram's own apps expose.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::limits::{
    ALBUM_MAX_ITEMS, CAPTION_MAX_UTF16, CAPTION_PREMIUM_MAX_UTF16, TEXT_MAX_UTF16,
};
use crate::{ChatId, ChatRef, Error, FormattedText, MessageId, Result};

/// One message (or one album) to send as the logged-in account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// Destination chat.
    pub to: ChatRef,
    /// What to send.
    pub content: Content,
    /// Delivery options.
    #[serde(default)]
    pub options: SendOptions,
}

/// Body of an outgoing message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// A text message.
    Text(FormattedText),
    /// One media item, or an album of up to [`ALBUM_MAX_ITEMS`], with one shared caption.
    Media {
        /// Items in display order.
        items: Vec<MediaItem>,
        /// Caption shown with the media; for an album Telegram attaches it to the first item.
        #[serde(default)]
        caption: FormattedText,
        /// Shows the caption above the media instead of below it.
        #[serde(default)]
        caption_above: bool,
    },
}

/// One photo or file inside [`Content::Media`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Where the bytes come from.
    pub source: MediaSource,
    /// How Telegram should treat the bytes.
    pub kind: MediaKind,
}

/// Origin of media bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaSource {
    /// A file on local disk.
    Path(PathBuf),
    /// Bytes already in memory, for example from an upload form. `name` drives the MIME type.
    Bytes {
        /// File name shown to the recipient for documents.
        name: String,
        /// File contents.
        #[serde(skip)]
        data: Vec<u8>,
    },
}

impl std::fmt::Debug for MediaSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(p) => f.debug_tuple("Path").field(p).finish(),
            // SECURITY: file contents stay out of logs and panic messages.
            Self::Bytes { name, data } => f
                .debug_struct("Bytes")
                .field("name", name)
                .field("len", &data.len())
                .finish(),
        }
    }
}

/// How Telegram presents a media item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaKind {
    /// A photo shown inline.
    Photo {
        /// Resolution handling.
        #[serde(default)]
        quality: PhotoQuality,
        /// Blurs the photo until the recipient taps it.
        #[serde(default)]
        spoiler: bool,
    },
    /// A file shown as a download, never recompressed.
    Document,
}

/// Photo resolution handling, matching the choice Telegram's apps offer.
///
/// TELEGRAM: layer 229 has no "HD" flag. The apps' HD toggle only means the client uploads the
/// image without shrinking it; the server keeps sizes up to
/// [`PHOTO_HD_MAX_SIDE_PX`](crate::limits::PHOTO_HD_MAX_SIDE_PX) on the longest side. Sending
/// the untouched original requires sending it as a document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhotoQuality {
    /// Upload the image as is and let Telegram keep its high-resolution size.
    #[default]
    Hd,
    /// Send the exact file as a document so nothing is recompressed.
    Original,
}

/// Delivery options for an outgoing message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendOptions {
    /// Replies to this message in the destination chat.
    #[serde(default)]
    pub reply_to: Option<MessageId>,
    /// Sends without a notification sound on the recipient's side.
    #[serde(default)]
    pub silent: bool,
    /// Suppresses the link preview Telegram would generate for URLs in the text.
    #[serde(default)]
    pub no_link_preview: bool,
    /// Allows the longer caption limit; only valid when the account has Telegram Premium.
    #[serde(default)]
    pub premium_caption: bool,
}

/// A message Telegram confirmed as sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentMessage {
    /// Chat the message landed in.
    pub chat_id: ChatId,
    /// Id Telegram assigned.
    pub id: MessageId,
    /// Raw TL object Telegram returned, for inspection.
    pub raw: serde_json::Value,
}

impl OutgoingMessage {
    /// A plain or styled text message to `to`.
    pub fn text(to: ChatRef, text: impl Into<FormattedText>) -> Self {
        Self {
            to,
            content: Content::Text(text.into()),
            options: SendOptions::default(),
        }
    }

    /// Checks the message against Telegram limits before any network call.
    ///
    /// # Errors
    ///
    /// Returns the first rule the message breaks: empty content, text or caption too long,
    /// entities out of range, an album over [`ALBUM_MAX_ITEMS`], or an album mixing photos and
    /// documents.
    pub fn validate(&self) -> Result<()> {
        match &self.content {
            Content::Text(text) => {
                if text.is_blank() {
                    return Err(Error::Empty);
                }
                text.validate("text", TEXT_MAX_UTF16)
            }
            Content::Media { items, caption, .. } => {
                if items.is_empty() {
                    return Err(Error::Empty);
                }
                if items.len() > ALBUM_MAX_ITEMS {
                    return Err(Error::AlbumTooLarge {
                        count: items.len(),
                        max: ALBUM_MAX_ITEMS,
                    });
                }
                // TELEGRAM: an album is either all photos (and videos) or all documents.
                let photos = items.iter().filter(|i| i.is_photo()).count();
                if photos != 0 && photos != items.len() {
                    return Err(Error::AlbumMixedKinds);
                }
                let max = if self.options.premium_caption {
                    CAPTION_PREMIUM_MAX_UTF16
                } else {
                    CAPTION_MAX_UTF16
                };
                caption.validate("caption", max)
            }
        }
    }
}

impl MediaItem {
    /// An HD photo from a local path.
    pub fn photo(path: impl Into<PathBuf>) -> Self {
        Self {
            source: MediaSource::Path(path.into()),
            kind: MediaKind::Photo {
                quality: PhotoQuality::Hd,
                spoiler: false,
            },
        }
    }

    /// A document from a local path.
    pub fn document(path: impl Into<PathBuf>) -> Self {
        Self {
            source: MediaSource::Path(path.into()),
            kind: MediaKind::Document,
        }
    }

    /// Whether Telegram will show this item as an inline photo. An original-quality photo is
    /// sent as a document, so it is not.
    pub fn is_photo(&self) -> bool {
        matches!(
            self.kind,
            MediaKind::Photo {
                quality: PhotoQuality::Hd,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntityKind;

    fn media(items: Vec<MediaItem>, caption: &str) -> OutgoingMessage {
        OutgoingMessage {
            to: ChatRef::Me,
            content: Content::Media {
                items,
                caption: caption.into(),
                caption_above: false,
            },
            options: SendOptions::default(),
        }
    }

    #[test]
    fn blank_text_is_rejected() {
        assert_eq!(
            OutgoingMessage::text(ChatRef::Me, "  ").validate(),
            Err(Error::Empty)
        );
    }

    #[test]
    fn photo_with_styled_caption_is_valid() {
        let mut msg = media(vec![MediaItem::photo("a.jpg")], "");
        if let Content::Media { caption, .. } = &mut msg.content {
            *caption = FormattedText::plain("Kỳ ").push("tháng 9", [EntityKind::Bold]);
        }
        msg.validate().unwrap();
    }

    #[test]
    fn caption_limit_depends_on_premium() {
        let long = "a".repeat(CAPTION_MAX_UTF16 + 1);
        let mut msg = media(vec![MediaItem::photo("a.jpg")], &long);
        assert!(matches!(
            msg.validate(),
            Err(Error::TooLong {
                what: "caption",
                ..
            })
        ));
        msg.options.premium_caption = true;
        msg.validate().unwrap();
    }

    #[test]
    fn album_rules() {
        let eleven = (0..=ALBUM_MAX_ITEMS)
            .map(|i| MediaItem::photo(format!("{i}.jpg")))
            .collect();
        assert!(matches!(
            media(eleven, "").validate(),
            Err(Error::AlbumTooLarge { .. })
        ));

        let mixed = vec![MediaItem::photo("a.jpg"), MediaItem::document("b.pdf")];
        assert_eq!(media(mixed, "").validate(), Err(Error::AlbumMixedKinds));

        let mut original = MediaItem::photo("a.png");
        original.kind = MediaKind::Photo {
            quality: PhotoQuality::Original,
            spoiler: false,
        };
        let docs = vec![original, MediaItem::document("b.pdf")];
        media(docs, "hai file").validate().unwrap();
    }

    #[test]
    fn bytes_source_debug_hides_contents() {
        let src = MediaSource::Bytes {
            name: "a.png".into(),
            data: vec![1, 2, 3],
        };
        assert_eq!(format!("{src:?}"), r#"Bytes { name: "a.png", len: 3 }"#);
    }
}
