//! Reaction emoji Telegram accepts from a bot, and the Zalo icon codes that map onto them.
//!
//! TELEGRAM: `setMessageReaction` rejects anything outside this set with `Bad Request`.
//! The Zalo table is the one in `chat-bridges` `src/networks/zalo/reaction.ts`. An icon with
//! no permitted emoji is skipped, not sent.

use hub_core::Network;

/// Emoji a bot may pass to `setMessageReaction`.
pub const TELEGRAM_REACTIONS: &[&str] = &[
    "👍",
    "👎",
    "❤️",
    "🔥",
    "🥰",
    "👏",
    "😁",
    "🤔",
    "🤯",
    "😱",
    "🤬",
    "😢",
    "🎉",
    "🤩",
    "🤮",
    "💩",
    "🙏",
    "👌",
    "🕊",
    "🤡",
    "🥱",
    "🥴",
    "😍",
    "🐳",
    "❤‍🔥",
    "🌚",
    "🌭",
    "💯",
    "🤣",
    "⚡",
    "🍌",
    "🏆",
    "💔",
    "🤨",
    "😐",
    "🍓",
    "🍾",
    "💋",
    "🖕",
    "😈",
    "😴",
    "😭",
    "🤓",
    "👻",
    "👨‍💻",
    "👀",
    "🎃",
    "🙈",
    "😇",
    "😨",
    "🤝",
    "✍",
    "🤗",
    "🫡",
    "🎅",
    "🎄",
    "☃",
    "💅",
    "🤪",
    "🗿",
    "🆒",
    "💘",
    "🙉",
    "🦄",
    "😘",
    "💊",
    "🙊",
    "😎",
    "👾",
    "🤷‍♂",
    "🤷",
    "🤷‍♀",
    "😡",
];

/// Zalo icon code, then the permitted Telegram emoji closest to it.
const ZALO_TO_TELEGRAM: &[(&str, &str)] = &[
    ("/-heart", "❤️"),
    ("/-strong", "👍"),
    ("/-weak", "👎"),
    (":>", "😁"),
    (":o", "😱"),
    (":-((", "😢"),
    (":((", "😭"),
    (":-h", "😡"),
    (":-*", "😘"),
    (";')", "🤣"),
    ("':>", "🤣"),
    ("/-shit", "💩"),
    ("/-rose", "🥰"),
    ("/-break", "💔"),
    (";xx", "🥰"),
    (";-/", "🤔"),
    (";-)", "😁"),
    ("/-fade", "⚡"),
    ("/-ok", "👌"),
    ("/-v", "👌"),
    ("/-thanks", "🙏"),
    ("_()_", "🙏"),
    ("/-punch", "🔥"),
    ("/-no", "🤷"),
    ("/-loveu", "🤝"),
    ("--b", "😐"),
    ("x-)", "😎"),
    ("/-bd", "🎉"),
];

/// Telegram emoji, then the Zalo icon the bridge sends back.
const TELEGRAM_TO_ZALO: &[(&str, &str)] = &[
    ("❤️", "/-heart"),
    ("❤", "/-heart"),
    ("🔥", "/-heart"),
    ("👍", "/-strong"),
    ("👎", "/-weak"),
    ("😁", ":>"),
    ("😱", ":o"),
    ("🤯", ":o"),
    ("😢", ":-(("),
    ("😭", ":(("),
    ("😡", ":-h"),
    ("🤬", ":-h"),
    ("😘", ":-*"),
    ("🥰", ";xx"),
    ("😍", ";xx"),
    ("🤣", ";')"),
    ("💩", "/-shit"),
    ("💔", "/-break"),
    ("🤔", ";-/"),
    ("🤨", ";-/"),
    ("👌", "/-ok"),
    ("🙏", "_()_"),
    ("🤷", "/-no"),
    ("🤝", "/-loveu"),
    ("😐", "--b"),
    ("😎", "x-)"),
    ("🤩", "x-)"),
    ("🎉", "/-bd"),
    ("💯", "/-strong"),
    ("👏", "/-strong"),
];

/// Whether `emoji` is in [`TELEGRAM_REACTIONS`].
pub fn is_telegram_reaction(emoji: &str) -> bool {
    TELEGRAM_REACTIONS.contains(&emoji)
}

/// Telegram emoji for a source reaction, or `None` when it must not be sent.
pub fn to_telegram_reaction(raw: &str) -> Option<&'static str> {
    if let Some(emoji) = TELEGRAM_REACTIONS
        .iter()
        .copied()
        .find(|emoji| *emoji == raw)
    {
        return Some(emoji);
    }
    ZALO_TO_TELEGRAM
        .iter()
        .find(|(icon, _)| *icon == raw)
        .map(|(_, emoji)| *emoji)
        .filter(|emoji| is_telegram_reaction(emoji))
}

/// Network reaction code for a Telegram emoji.
///
/// Zalo gets an icon code. Messenger and Telegram keep the emoji when it is permitted.
pub fn to_network_reaction(network: Network, emoji: &str) -> Option<String> {
    match network {
        Network::Zalo => TELEGRAM_TO_ZALO
            .iter()
            .find(|(candidate, _)| *candidate == emoji)
            .map(|(_, icon)| (*icon).to_owned()),
        Network::Messenger | Network::Telegram => {
            is_telegram_reaction(emoji).then(|| emoji.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: &[&str] = &[
        "👍",
        "👎",
        "❤️",
        "🔥",
        "🥰",
        "👏",
        "😁",
        "🤔",
        "🤯",
        "😱",
        "🤬",
        "😢",
        "🎉",
        "🤩",
        "🤮",
        "💩",
        "🙏",
        "👌",
        "🕊",
        "🤡",
        "🥱",
        "🥴",
        "😍",
        "🐳",
        "❤‍🔥",
        "🌚",
        "🌭",
        "💯",
        "🤣",
        "⚡",
        "🍌",
        "🏆",
        "💔",
        "🤨",
        "😐",
        "🍓",
        "🍾",
        "💋",
        "🖕",
        "😈",
        "😴",
        "😭",
        "🤓",
        "👻",
        "👨‍💻",
        "👀",
        "🎃",
        "🙈",
        "😇",
        "😨",
        "🤝",
        "✍",
        "🤗",
        "🫡",
        "🎅",
        "🎄",
        "☃",
        "💅",
        "🤪",
        "🗿",
        "🆒",
        "💘",
        "🙉",
        "🦄",
        "😘",
        "💊",
        "🙊",
        "😎",
        "👾",
        "🤷‍♂",
        "🤷",
        "🤷‍♀",
        "😡",
    ];

    #[test]
    fn allowed_set_matches_the_telegram_bot_list() {
        assert_eq!(TELEGRAM_REACTIONS, EXPECTED);
        assert_eq!(TELEGRAM_REACTIONS.len(), 73);
    }

    #[test]
    fn zalo_icons_only_land_on_permitted_emoji() {
        assert_eq!(to_telegram_reaction("/-heart"), Some("❤️"));
        assert_eq!(to_telegram_reaction("/-strong"), Some("👍"));
        assert_eq!(to_telegram_reaction("/-weak"), Some("👎"));
        for (icon, emoji) in ZALO_TO_TELEGRAM {
            let mapped = to_telegram_reaction(icon);
            assert_eq!(mapped, Some(*emoji), "{icon}");
            assert!(is_telegram_reaction(emoji), "{emoji}");
        }
        for banned in ["🌹", "✨", "✌️", "👊", "😂"] {
            assert!(!is_telegram_reaction(banned), "{banned}");
            assert_eq!(to_telegram_reaction(banned), None);
        }
        assert_eq!(to_telegram_reaction("/-brand-new-2027"), None);
        assert_eq!(to_telegram_reaction(""), None);
    }

    #[test]
    fn core_reactions_round_trip_to_zalo() {
        for icon in ["/-heart", "/-strong", "/-weak", ":-*", "/-break"] {
            let emoji = to_telegram_reaction(icon).unwrap();
            assert_eq!(
                to_network_reaction(Network::Zalo, emoji).as_deref(),
                Some(icon)
            );
        }
        assert_eq!(
            to_network_reaction(Network::Zalo, "❤").as_deref(),
            Some("/-heart")
        );
        assert_eq!(
            to_network_reaction(Network::Zalo, "❤️").as_deref(),
            Some("/-heart")
        );
        assert_eq!(to_network_reaction(Network::Zalo, "🦄"), None);
        assert_eq!(to_network_reaction(Network::Zalo, "😂"), None);
        assert_eq!(
            to_network_reaction(Network::Messenger, "👍").as_deref(),
            Some("👍")
        );
    }
}
