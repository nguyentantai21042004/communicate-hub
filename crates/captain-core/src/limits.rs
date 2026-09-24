//! Limits Telegram enforces on content, checked locally before any request is sent.
//!
//! TELEGRAM: the numbers are the published client limits and the layer 229 TL schema. Recheck
//! them whenever the pinned grammers release moves to a new layer. Lengths are counted in UTF-16
//! code units, the unit MTProto uses for entity offsets. That is the conservative reading:
//! an emoji outside the BMP counts as two units.
#![allow(missing_docs)]

pub const TEXT_MAX_UTF16: usize = 4096; // messages.sendMessage text
pub const CAPTION_MAX_UTF16: usize = 1024; // media caption without Telegram Premium
pub const CAPTION_PREMIUM_MAX_UTF16: usize = 4096; // media caption with Telegram Premium
pub const ALBUM_MAX_ITEMS: usize = 10; // messages.sendMultiMedia multi_media vector
pub const PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024; // larger images must be sent as a document
pub const PHOTO_HD_MAX_SIDE_PX: u32 = 2560; // largest side the server keeps for a photo
