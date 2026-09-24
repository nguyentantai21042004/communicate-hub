//! Text with styling entities, in the exact shape MTProto sends and receives.
//!
//! TELEGRAM: styling is not markup inside the string. It is a list of entities, each an
//! `(offset, length)` range counted in UTF-16 code units over the plain text. This module keeps
//! that model so the protocol layer converts it one-to-one; parsing Markdown or HTML into it is
//! the job of `captain-format`.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Plain text plus the entities that style it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattedText {
    /// The text exactly as the recipient sees it, without any markup.
    pub text: String,
    /// Styling ranges over `text`. Order does not matter; ranges may nest or overlap.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<TextEntity>,
}

/// One styled range of a [`FormattedText`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEntity {
    /// Start of the range, in UTF-16 code units from the beginning of the text.
    pub offset_utf16: usize,
    /// Length of the range, in UTF-16 code units.
    pub length_utf16: usize,
    /// What the range means.
    pub kind: EntityKind,
}

/// Entity kinds a user account can send. Kinds Telegram detects on its own (mentions, hashtags,
/// plain URLs) only appear on inbound messages as [`EntityKind::Other`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntityKind {
    /// Bold text.
    Bold,
    /// Italic text.
    Italic,
    /// Underlined text.
    Underline,
    /// Struck-through text.
    Strikethrough,
    /// Text hidden until the recipient taps it.
    Spoiler,
    /// Inline monospace.
    Code,
    /// Monospace block, optionally tagged with a language for syntax highlighting.
    Pre {
        /// Language name such as `rust`, empty when unknown.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        language: String,
    },
    /// Text that opens `url` when tapped.
    TextUrl {
        /// Target URL.
        url: String,
    },
    /// Mention of a user by id, for users without a username.
    MentionUser {
        /// Telegram user id.
        user_id: i64,
    },
    /// Quote block; `collapsed` shows only the first lines until expanded.
    Blockquote {
        /// Whether the quote starts collapsed.
        #[serde(default)]
        collapsed: bool,
    },
    /// Premium custom emoji replacing the fallback emoji in the text.
    CustomEmoji {
        /// Telegram document id of the custom emoji.
        document_id: i64,
    },
    /// Any entity this layer does not model, kept by its TL constructor name.
    Other {
        /// TL constructor name, for example `messageEntityHashtag`.
        name: String,
    },
}

impl FormattedText {
    /// Text without any styling.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            entities: Vec::new(),
        }
    }

    /// Whether there is no visible text.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Length of the text in UTF-16 code units, the unit Telegram limits and offsets use.
    pub fn len_utf16(&self) -> usize {
        utf16_len(&self.text)
    }

    /// Appends `segment` and styles exactly that segment with `kinds`, computing UTF-16 offsets.
    ///
    /// This is the safe way to build entities by hand: offsets can never drift from the text.
    #[must_use]
    pub fn push(mut self, segment: &str, kinds: impl IntoIterator<Item = EntityKind>) -> Self {
        let offset_utf16 = self.len_utf16();
        let length_utf16 = utf16_len(segment);
        self.text.push_str(segment);
        if length_utf16 > 0 {
            self.entities
                .extend(kinds.into_iter().map(|kind| TextEntity {
                    offset_utf16,
                    length_utf16,
                    kind,
                }));
        }
        self
    }

    /// Checks that every entity lies inside the text and the text fits in `max_utf16`.
    ///
    /// # Errors
    ///
    /// [`Error::TooLong`] when the text exceeds `max_utf16`, and [`Error::EntityOutOfRange`]
    /// when an entity ends past the text.
    pub fn validate(&self, what: &'static str, max_utf16: usize) -> Result<()> {
        let text_len_utf16 = self.len_utf16();
        if text_len_utf16 > max_utf16 {
            return Err(Error::TooLong {
                what,
                len_utf16: text_len_utf16,
                max_utf16,
            });
        }
        for e in &self.entities {
            if e.offset_utf16 + e.length_utf16 > text_len_utf16 {
                return Err(Error::EntityOutOfRange {
                    offset_utf16: e.offset_utf16,
                    length_utf16: e.length_utf16,
                    text_len_utf16,
                });
            }
        }
        Ok(())
    }

    /// Returns the substring an entity covers, or `None` if its range does not fall on character
    /// boundaries of this text.
    pub fn slice(&self, entity: &TextEntity) -> Option<&str> {
        let start = byte_index(&self.text, entity.offset_utf16)?;
        let end = byte_index(&self.text, entity.offset_utf16 + entity.length_utf16)?;
        self.text.get(start..end)
    }
}

impl From<&str> for FormattedText {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

impl From<String> for FormattedText {
    fn from(text: String) -> Self {
        Self::plain(text)
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Byte index in `s` that corresponds to `offset_utf16`, if it falls between two characters.
fn byte_index(s: &str, offset_utf16: usize) -> Option<usize> {
    let mut units = 0;
    for (byte, ch) in s.char_indices() {
        if units == offset_utf16 {
            return Some(byte);
        }
        units += ch.len_utf16();
    }
    (units == offset_utf16).then_some(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vietnamese_diacritics_are_one_unit_each() {
        // Precomposed Vietnamese letters sit in the BMP, so bytes and UTF-16 units differ.
        let t = FormattedText::plain("Đã deploy");
        assert_eq!(t.len_utf16(), 9);
        assert_eq!(t.text.len(), 11);
    }

    #[test]
    fn push_keeps_offsets_right_after_emoji() {
        let t = FormattedText::plain("🚀 ")
            .push("Đã deploy", [EntityKind::Bold])
            .push(" v2.14", [EntityKind::Code, EntityKind::Italic]);
        assert_eq!(t.text, "🚀 Đã deploy v2.14");
        // The rocket is outside the BMP: two UTF-16 units, then one for the space.
        assert_eq!(t.entities[0].offset_utf16, 3);
        assert_eq!(t.entities[0].length_utf16, 9);
        assert_eq!(t.slice(&t.entities[0]), Some("Đã deploy"));
        assert_eq!(t.slice(&t.entities[1]), Some(" v2.14"));
        assert_eq!(t.entities.len(), 3);
        t.validate("text", 4096).unwrap();
    }

    #[test]
    fn empty_segment_adds_no_entity() {
        let t = FormattedText::plain("a").push("", [EntityKind::Bold]);
        assert!(t.entities.is_empty());
    }

    #[test]
    fn validate_rejects_out_of_range_and_too_long() {
        let mut t = FormattedText::plain("abc");
        t.entities.push(TextEntity {
            offset_utf16: 2,
            length_utf16: 5,
            kind: EntityKind::Bold,
        });
        assert!(matches!(
            t.validate("text", 10),
            Err(Error::EntityOutOfRange { .. })
        ));
        assert!(matches!(
            FormattedText::plain("abcd").validate("caption", 3),
            Err(Error::TooLong {
                len_utf16: 4,
                max_utf16: 3,
                ..
            })
        ));
    }

    #[test]
    fn slice_refuses_to_split_a_surrogate_pair() {
        let t = FormattedText::plain("🚀x");
        let half = TextEntity {
            offset_utf16: 1,
            length_utf16: 2,
            kind: EntityKind::Bold,
        };
        assert_eq!(t.slice(&half), None);
    }

    #[test]
    fn serializes_entities_with_a_type_tag() {
        let t = FormattedText::plain("x").push(
            "y",
            [EntityKind::TextUrl {
                url: "https://a.b".into(),
            }],
        );
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["entities"][0]["kind"]["type"], "text_url");
    }
}
