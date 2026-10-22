//! Telegram HTML and topic titles.
//!
//! Text and captions are measured in UTF-16 code units, which is how Telegram counts. Topic
//! titles use the same unit because the Bot API caps `createForumTopic` at 128 of them.

use hub_core::{ConversationKind, Mention, Network, limits};

use crate::limits::{
    DEFAULT_SENDER_NAME, FALLBACK_TOPIC_NAME, SENDER_NAME_MAX_CHARS, TOPIC_TITLE_MAX_CHARS,
};

const ELLIPSIS: char = '\u{2026}';
const ZALO_TOPIC_PREFIX: &str = "\u{1F535}"; // 🔵
const MESSENGER_TOPIC_PREFIX: &str = "\u{1F7E3}"; // 🟣
const TELEGRAM_TOPIC_PREFIX: &str = "\u{2708}\u{FE0F}"; // ✈️
const PERSON_MARK: &str = "\u{1F464}"; // 👤 hub-tgbot matches this scalar
const GROUP_MARK: &str = "\u{1F465}"; // 👥 hub-tgbot matches this scalar

/// Escapes `&`, `<`, and `>` for Telegram HTML.
///
/// Quotes are left alone. Telegram's HTML mode does not treat them as markup.
pub(crate) fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// UTF-16 length of `text`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Truncates `text` to `max_units` UTF-16 code units, ending with `…` when it cuts.
pub(crate) fn truncate_utf16(text: &str, max_units: usize) -> String {
    if utf16_len(text) <= max_units {
        return text.to_owned();
    }
    if max_units == 0 {
        return String::new();
    }
    let budget = max_units - 1;
    let mut units = 0;
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        let next = units + ch.len_utf16();
        if next > budget {
            break;
        }
        units = next;
        end = index + ch.len_utf8();
    }
    let mut out = text[..end].to_owned();
    out.push(ELLIPSIS);
    out
}

/// Collapses whitespace to single spaces and trims the ends.
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Forum topic title: a network emoji, a person or group mark, then the name.
pub(crate) fn topic_title(network: Network, kind: ConversationKind, display_name: &str) -> String {
    let prefix = network_prefix(network);
    let mark = kind_mark(kind);
    let compact = collapse_whitespace(display_name);
    let body = if compact.is_empty() {
        FALLBACK_TOPIC_NAME
    } else {
        compact.as_str()
    };
    truncate_utf16(&format!("{prefix} {mark} {body}"), TOPIC_TITLE_MAX_CHARS)
}

fn kind_mark(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Dm => PERSON_MARK,
        ConversationKind::Group => GROUP_MARK,
    }
}

/// Emoji that prefixes a topic title for `network`.
pub(crate) fn network_prefix(network: Network) -> &'static str {
    match network {
        Network::Zalo => ZALO_TOPIC_PREFIX,
        Network::Messenger => MESSENGER_TOPIC_PREFIX,
        Network::Telegram => TELEGRAM_TOPIC_PREFIX,
    }
}

/// `<b>Sender:</b>` plus the escaped body, clipped to [`limits::TEXT_MAX_UTF16`].
pub(crate) fn render_text_html(sender: &str, body: &str, mentions: &[Mention]) -> String {
    let name = truncate_utf16(
        if sender.trim().is_empty() {
            DEFAULT_SENDER_NAME
        } else {
            sender.trim()
        },
        SENDER_NAME_MAX_CHARS,
    );
    let header = format!("<b>{}:</b>\n", escape_html(&name));
    let budget = limits::TEXT_MAX_UTF16.saturating_sub(utf16_len(&header));
    let with_mentions = emphasize_mentions(body, mentions, budget);
    let full = format!("{header}{with_mentions}");
    if utf16_len(&full) <= limits::TEXT_MAX_UTF16 {
        return full;
    }
    format!("{header}{}", escape_html(&truncate_utf16(body, budget)))
}

/// Plain caption for [`hub_core::TopicSink::send_media`], clipped to the non-premium caption limit.
pub(crate) fn render_caption(sender: &str, body: &str) -> String {
    let name = truncate_utf16(
        if sender.trim().is_empty() {
            DEFAULT_SENDER_NAME
        } else {
            sender.trim()
        },
        SENDER_NAME_MAX_CHARS,
    );
    let plain = if body.trim().is_empty() {
        name
    } else {
        format!("{name}: {body}")
    };
    truncate_utf16(&plain, limits::CAPTION_MAX_UTF16)
}

/// Italic notice, clipped to the text limit.
pub(crate) fn notice_html(text: &str) -> String {
    let inner_budget = limits::TEXT_MAX_UTF16.saturating_sub(utf16_len("<i></i>"));
    format!(
        "<i>{}</i>",
        escape_html(&truncate_utf16(text, inner_budget))
    )
}

/// Bolds the first occurrence of each mention display name that still fits in `budget`.
fn emphasize_mentions(body: &str, mentions: &[Mention], budget: usize) -> String {
    let clipped = truncate_utf16(body, budget);
    if mentions.is_empty() {
        return escape_html(&clipped);
    }
    let mut ranges = Vec::new();
    for mention in mentions {
        let name = mention.display_name.trim();
        if name.is_empty() {
            continue;
        }
        if let Some(start) = clipped.find(name) {
            ranges.push((start, start + name.len()));
        }
    }
    if ranges.is_empty() {
        return escape_html(&clipped);
    }
    ranges.sort_unstable();
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start < last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    let mut out = String::new();
    let mut cursor = 0;
    for (start, end) in merged {
        if start < cursor {
            continue;
        }
        out.push_str(&escape_html(&clipped[cursor..start]));
        out.push_str("<b>");
        out.push_str(&escape_html(&clipped[start..end]));
        out.push_str("</b>");
        cursor = end;
    }
    out.push_str(&escape_html(&clipped[cursor..]));
    out
}

#[cfg(test)]
mod tests {
    use hub_core::ConversationKind;

    use super::*;

    #[test]
    fn escape_html_leaves_quotes_alone() {
        assert_eq!(escape_html("<b>a & b</b>"), "&lt;b&gt;a &amp; b&lt;/b&gt;");
        assert_eq!(
            escape_html("he said \"hi\" & 'bye'"),
            "he said \"hi\" &amp; 'bye'"
        );
    }

    #[test]
    fn truncate_marks_a_cut_and_stays_inside_the_limit() {
        assert_eq!(truncate_utf16("abc", 3), "abc");
        let cut = truncate_utf16("abcdef", 4);
        assert_eq!(cut, "abc\u{2026}");
        assert_eq!(utf16_len(&cut), 4);
        let long = truncate_utf16(
            &"x".repeat(limits::TEXT_MAX_UTF16 + 50),
            limits::TEXT_MAX_UTF16,
        );
        assert_eq!(utf16_len(&long), limits::TEXT_MAX_UTF16);
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn topic_title_prefixes_the_network_and_clips() {
        assert_eq!(
            topic_title(Network::Zalo, ConversationKind::Group, "Nhóm giao hàng"),
            "🔵 👥 Nhóm giao hàng"
        );
        assert_eq!(
            topic_title(Network::Messenger, ConversationKind::Dm, "Tuấn"),
            "🟣 👤 Tuấn"
        );
        assert!(topic_title(Network::Telegram, ConversationKind::Group, "ops").starts_with("✈️"));
        assert!(topic_title(Network::Telegram, ConversationKind::Group, "ops").contains("👥"));
        assert_eq!(
            topic_title(Network::Zalo, ConversationKind::Dm, "   "),
            "🔵 👤 chat"
        );
        let long = topic_title(Network::Zalo, ConversationKind::Dm, &"y".repeat(200));
        assert!(utf16_len(&long) <= TOPIC_TITLE_MAX_CHARS);
        assert!(long.ends_with('\u{2026}'));
        assert_eq!(
            topic_title(
                Network::Zalo,
                ConversationKind::Group,
                "  Nhóm   giao  hàng "
            ),
            "🔵 👥 Nhóm giao hàng"
        );
    }

    #[test]
    fn render_bolds_a_mention_and_escapes_the_sender() {
        let html = render_text_html(
            "A & B",
            "hi Tài",
            &[Mention {
                user_id: "u-1".into(),
                display_name: "Tài".into(),
            }],
        );
        assert_eq!(html, "<b>A &amp; B:</b>\nhi <b>Tài</b>");
    }
}
