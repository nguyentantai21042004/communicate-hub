//! Masking for Zalo ids on list responses.
//!
//! Telegram ids are left intact by the caller. A scope snippet is the one place a page needs
//! an exact id, and that snippet is Telegram-only.

const MASK_EDGE: usize = 4; // characters kept at each end
const MASK_MARK: char = '…'; // stands in for the hidden middle
const MASK_BULLET: char = '•'; // replaces an id too short to split

/// Masks a Zalo user id or chat id.
///
/// Keeps the first four and last four Unicode scalars when the id is longer than eight.
/// A shorter id would be revealed by showing both ends, so every scalar becomes a bullet.
/// An empty id becomes one bullet.
pub fn mask_external_id(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() <= MASK_EDGE * 2 {
        let width = chars.len().max(1);
        return std::iter::repeat_n(MASK_BULLET, width).collect();
    }
    let head: String = chars[..MASK_EDGE].iter().collect();
    let tail: String = chars[chars.len() - MASK_EDGE..].iter().collect();
    format!("{head}{MASK_MARK}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_ids_keep_both_ends_and_short_ids_do_not() {
        assert_eq!(mask_external_id("1234567890"), "1234…7890");
        assert_eq!(mask_external_id("abcd😊wxyz"), "abcd…wxyz");
        assert_eq!(mask_external_id("abcdefgh"), "••••••••");
        assert_eq!(mask_external_id("abc"), "•••");
        assert_eq!(mask_external_id(""), "•");
    }
}
