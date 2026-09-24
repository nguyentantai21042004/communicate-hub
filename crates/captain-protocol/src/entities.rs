//! One-to-one conversion between core [`TextEntity`] values and TL `MessageEntity`.
//!
//! TELEGRAM: offsets and lengths are UTF-16 code units on both sides, so they pass through
//! unchanged. Only the representation differs.

use captain_core::{EntityKind, FormattedText, TextEntity};
use grammers_client::tl;

/// Converts inbound TL entities. Kinds the core does not model become [`EntityKind::Other`].
pub(crate) fn from_tl(
    text: &str,
    entities: Option<&Vec<tl::enums::MessageEntity>>,
) -> FormattedText {
    let entities = entities.map(|list| list.iter().filter_map(entity_from_tl).collect());
    FormattedText {
        text: text.to_owned(),
        entities: entities.unwrap_or_default(),
    }
}

/// Converts outbound entities. `Other` kinds are dropped: they are either detected by the server
/// (mentions, hashtags, plain URLs) or unknown to this layer, so there is nothing to send.
pub(crate) fn to_tl(entities: &[TextEntity]) -> Vec<tl::enums::MessageEntity> {
    entities.iter().filter_map(entity_to_tl).collect()
}

fn entity_from_tl(entity: &tl::enums::MessageEntity) -> Option<TextEntity> {
    use tl::enums::MessageEntity as E;

    let (offset, length, kind) = match entity {
        E::Bold(e) => (e.offset, e.length, EntityKind::Bold),
        E::Italic(e) => (e.offset, e.length, EntityKind::Italic),
        E::Underline(e) => (e.offset, e.length, EntityKind::Underline),
        E::Strike(e) => (e.offset, e.length, EntityKind::Strikethrough),
        E::Spoiler(e) => (e.offset, e.length, EntityKind::Spoiler),
        E::Code(e) => (e.offset, e.length, EntityKind::Code),
        E::Pre(e) => (
            e.offset,
            e.length,
            EntityKind::Pre {
                language: e.language.clone(),
            },
        ),
        E::TextUrl(e) => (
            e.offset,
            e.length,
            EntityKind::TextUrl { url: e.url.clone() },
        ),
        E::MentionName(e) => (
            e.offset,
            e.length,
            EntityKind::MentionUser { user_id: e.user_id },
        ),
        E::Blockquote(e) => (
            e.offset,
            e.length,
            EntityKind::Blockquote {
                collapsed: e.collapsed,
            },
        ),
        E::CustomEmoji(e) => (
            e.offset,
            e.length,
            EntityKind::CustomEmoji {
                document_id: e.document_id,
            },
        ),
        E::Mention(e) => (e.offset, e.length, other("messageEntityMention")),
        E::Hashtag(e) => (e.offset, e.length, other("messageEntityHashtag")),
        E::Cashtag(e) => (e.offset, e.length, other("messageEntityCashtag")),
        E::BotCommand(e) => (e.offset, e.length, other("messageEntityBotCommand")),
        E::Url(e) => (e.offset, e.length, other("messageEntityUrl")),
        E::Email(e) => (e.offset, e.length, other("messageEntityEmail")),
        E::Phone(e) => (e.offset, e.length, other("messageEntityPhone")),
        E::BankCard(e) => (e.offset, e.length, other("messageEntityBankCard")),
        E::Unknown(e) => (e.offset, e.length, other("messageEntityUnknown")),
        // Remaining constructors are outbound-only or newer than this mapping; `raw` keeps them.
        _ => return None,
    };
    Some(TextEntity {
        offset_utf16: to_usize(offset),
        length_utf16: to_usize(length),
        kind,
    })
}

fn entity_to_tl(entity: &TextEntity) -> Option<tl::enums::MessageEntity> {
    use tl::enums::MessageEntity as E;
    use tl::types as T;

    // Lengths were validated against TEXT_MAX_UTF16, far below i32::MAX; saturate rather than
    // wrap if a caller skipped validation, so the server rejects it instead of misstyling.
    let offset = i32::try_from(entity.offset_utf16).unwrap_or(i32::MAX);
    let length = i32::try_from(entity.length_utf16).unwrap_or(i32::MAX);
    Some(match &entity.kind {
        EntityKind::Bold => E::Bold(T::MessageEntityBold { offset, length }),
        EntityKind::Italic => E::Italic(T::MessageEntityItalic { offset, length }),
        EntityKind::Underline => E::Underline(T::MessageEntityUnderline { offset, length }),
        EntityKind::Strikethrough => E::Strike(T::MessageEntityStrike { offset, length }),
        EntityKind::Spoiler => E::Spoiler(T::MessageEntitySpoiler { offset, length }),
        EntityKind::Code => E::Code(T::MessageEntityCode { offset, length }),
        EntityKind::Pre { language } => E::Pre(T::MessageEntityPre {
            offset,
            length,
            language: language.clone(),
        }),
        EntityKind::TextUrl { url } => E::TextUrl(T::MessageEntityTextUrl {
            offset,
            length,
            url: url.clone(),
        }),
        // TELEGRAM: the server only accepts inputMessageEntityMentionName, which needs the
        // user's access hash. grammers swaps this constructor for it on send, using the cache.
        EntityKind::MentionUser { user_id } => E::MentionName(T::MessageEntityMentionName {
            offset,
            length,
            user_id: *user_id,
        }),
        EntityKind::Blockquote { collapsed } => E::Blockquote(T::MessageEntityBlockquote {
            collapsed: *collapsed,
            offset,
            length,
        }),
        EntityKind::CustomEmoji { document_id } => E::CustomEmoji(T::MessageEntityCustomEmoji {
            offset,
            length,
            document_id: *document_id,
        }),
        EntityKind::Other { .. } => return None,
    })
}

fn other(name: &str) -> EntityKind {
    EntityKind::Other {
        name: name.to_owned(),
    }
}

/// Telegram never sends negative offsets; clamp to zero instead of wrapping if it ever does.
fn to_usize(n: i32) -> usize {
    usize::try_from(n).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_vietnamese_round_trips_with_utf16_offsets() {
        let text = FormattedText::plain("🚀 ")
            .push("Đã deploy", [EntityKind::Bold])
            .push(" ", [])
            .push("v2.14", [EntityKind::Code])
            .push(" ", [])
            .push(
                "log",
                [EntityKind::TextUrl {
                    url: "https://x.y/log".into(),
                }],
            );
        let tl_entities = to_tl(&text.entities);
        assert_eq!(tl_entities.len(), 3);
        let back = from_tl(&text.text, Some(&tl_entities));
        assert_eq!(back, text);
    }

    #[test]
    fn server_detected_entities_come_back_as_other() {
        let raw = vec![tl::enums::MessageEntity::Hashtag(
            tl::types::MessageEntityHashtag {
                offset: 0,
                length: 4,
            },
        )];
        let text = from_tl("#ops hi", Some(&raw));
        assert_eq!(text.entities[0].kind, other("messageEntityHashtag"));
        assert!(
            to_tl(&text.entities).is_empty(),
            "server-detected kinds are not sent back"
        );
    }

    #[test]
    fn no_entities_means_plain_text() {
        assert_eq!(from_tl("hi", None), FormattedText::plain("hi"));
    }
}
