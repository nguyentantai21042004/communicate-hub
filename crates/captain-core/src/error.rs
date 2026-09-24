//! Validation errors for values built from core types.

/// Why an outgoing message or a piece of formatted text was rejected before reaching Telegram.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Text and media are both missing, so there is nothing to send.
    #[error("message has no text and no media")]
    Empty,
    /// Text or caption exceeds the Telegram limit for its kind.
    #[error("{what} is {len_utf16} UTF-16 units long, limit is {max_utf16}")]
    TooLong {
        /// Which field was too long, for example "caption".
        what: &'static str,
        /// Measured length in UTF-16 code units.
        len_utf16: usize,
        /// Limit in UTF-16 code units.
        max_utf16: usize,
    },
    /// An album holds more items than Telegram accepts in one request.
    #[error("album has {count} items, limit is {max}")]
    AlbumTooLarge {
        /// Number of items given.
        count: usize,
        /// Maximum number of items.
        max: usize,
    },
    /// An album mixes photos with documents, which Telegram refuses.
    #[error("album mixes photos and documents; send them as separate albums")]
    AlbumMixedKinds,
    /// An entity points outside the text it formats.
    #[error(
        "entity at {offset_utf16}+{length_utf16} is outside text of {text_len_utf16} UTF-16 units"
    )]
    EntityOutOfRange {
        /// Entity start, UTF-16 code units.
        offset_utf16: usize,
        /// Entity length, UTF-16 code units.
        length_utf16: usize,
        /// Text length, UTF-16 code units.
        text_len_utf16: usize,
    },
    /// A chat reference could not be parsed.
    #[error("chat reference {0:?} is not @username, a numeric id, or \"me\"")]
    InvalidChatRef(String),
}

/// Result alias for core validation.
pub type Result<T> = std::result::Result<T, Error>;
