//! `@Name` in a Telegram message, and display names coming back from a network.
//!
//! Names are folded the way the TypeScript bridge folds them: lowercase, diacritics stripped,
//! `đ` mapped to `d`. The longest run of words the cache knows wins, so `@Nguyễn Tấn Tài` does
//! not resolve to a different person named `Tài`.

use unicode_normalization::UnicodeNormalization;

use crate::limits::MENTION_MAX_WORDS;

/// One `@Name` resolved to a network user id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedMention {
    /// Network user id.
    pub user_id: String,
    /// The words that matched, without the leading `@`.
    pub display_name: String,
}

/// Folded form used as the cache key.
pub(crate) fn normalize_name(name: &str) -> String {
    let lower = name.to_lowercase().replace('đ', "d");
    let stripped: String = lower.nfd().filter(|ch| !is_combining_mark(*ch)).collect();
    collapse(&stripped)
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

fn starts_mention(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(ch) => matches!(ch, ' ' | '\n' | '\t' | '(' | '['),
    }
}

/// Whether `chars[index]` starts an `@` mention and is not inside a word.
pub(crate) fn is_mention_at(chars: &[char], index: usize) -> bool {
    chars.get(index).copied() == Some('@')
        && starts_mention(index.checked_sub(1).and_then(|at| chars.get(at).copied()))
}

/// Next word prefix of a mention, from `start` through `cursor`.
///
/// `start` is the first character after `@`. `cursor` is where the previous prefix ended.
/// The returned string runs from `start` through the new word, and the `usize` is the next cursor.
pub(crate) fn next_prefix(
    chars: &[char],
    start: usize,
    mut cursor: usize,
) -> Option<(String, usize)> {
    while cursor < chars.len() && chars[cursor] == ' ' {
        cursor += 1;
    }
    let word_start = cursor;
    while cursor < chars.len() && !ends_word(chars[cursor]) {
        cursor += 1;
    }
    if cursor == word_start {
        return None;
    }
    Some((chars[start..cursor].iter().collect(), cursor))
}

fn ends_word(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\n' | '\t' | ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
    )
}

/// Resolves every `@Name` in `text`.
///
/// `lookup` receives the raw candidate, including spaces between words, and returns the
/// network user id. An `@` inside a word, such as an email address, is ignored.
// Production resolves mentions through the store. This sync walk is what the unit tests pin.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn resolve_mentions(
    text: &str,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Vec<ResolvedMention> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if !is_mention_at(&chars, index) {
            index += 1;
            continue;
        }
        let Some(found) = longest_match(&chars, index + 1, &mut lookup) else {
            index += 1;
            continue;
        };
        index += 1 + found.display_name.chars().count();
        out.push(found);
    }
    out
}

#[cfg_attr(not(test), allow(dead_code))]
fn longest_match(
    chars: &[char],
    start: usize,
    lookup: &mut impl FnMut(&str) -> Option<String>,
) -> Option<ResolvedMention> {
    let mut best: Option<ResolvedMention> = None;
    let mut cursor = start;
    for _ in 0..MENTION_MAX_WORDS {
        let Some((candidate, next)) = next_prefix(chars, start, cursor) else {
            break;
        };
        cursor = next;
        if let Some(user_id) = lookup(&candidate) {
            best = Some(ResolvedMention {
                user_id,
                display_name: candidate,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn roster() -> HashMap<String, String> {
        HashMap::from([
            ("nguyen tan tai".into(), "u-tai".into()),
            ("tai".into(), "u-other-tai".into()),
            ("tuan".into(), "u-tuan".into()),
        ])
    }

    fn lookup(name: &str) -> Option<String> {
        roster().get(&normalize_name(name)).cloned()
    }

    #[test]
    fn folds_case_and_diacritics() {
        assert_eq!(normalize_name("  Nguyễn   Tấn  "), "nguyen tan");
        assert_eq!(normalize_name("Đặng"), "dang");
        assert_eq!(normalize_name("TUẤN"), "tuan");
    }

    #[test]
    fn resolves_the_longest_name_and_skips_email() {
        let one = resolve_mentions("hi @Tuan nhé", lookup);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].user_id, "u-tuan");
        assert_eq!(one[0].display_name, "Tuan");

        let long = resolve_mentions("cc @Nguyễn Tấn Tài xem giúp", lookup);
        assert_eq!(long.len(), 1);
        assert_eq!(long[0].user_id, "u-tai");
        assert_eq!(long[0].display_name, "Nguyễn Tấn Tài");

        assert!(resolve_mentions("mail tai@tuan.vn", lookup).is_empty());
        assert!(resolve_mentions("hi @KhôngAiCả", lookup).is_empty());

        let many = resolve_mentions("@Tuan và @Tai cùng xem", lookup);
        assert_eq!(
            many.iter()
                .map(|item| item.user_id.as_str())
                .collect::<Vec<_>>(),
            ["u-tuan", "u-other-tai"]
        );

        assert_eq!(resolve_mentions("@TUẤN ơi", lookup)[0].user_id, "u-tuan");
        assert_eq!(resolve_mentions("@Tuan", lookup)[0].display_name, "Tuan");
        assert_eq!(resolve_mentions("(@Tuan)", lookup)[0].display_name, "Tuan");
        assert_eq!(
            resolve_mentions("@Tuan, xem nhé", lookup)[0].display_name,
            "Tuan"
        );
    }
}
