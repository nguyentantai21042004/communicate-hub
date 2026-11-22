//! Identifiers for chats and messages.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const CHANNEL_MARK: i64 = 1_000_000_000_000; // marked channel id = -(CHANNEL_MARK + channel id)

/// Marked chat id, as used by the Bot API and Telethon: users are positive, basic groups are
/// negative, and channels or supergroups are `-100` followed by the channel id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChatId(pub i64);

impl ChatId {
    /// The bare channel id of a channel or supergroup, as used in `t.me/c/` links, or `None`
    /// for users and basic groups.
    pub fn channel_id(self) -> Option<i64> {
        (self.0 < -CHANNEL_MARK).then(|| -self.0 - CHANNEL_MARK)
    }
}

/// Message id, unique within one chat (or within the account for private chats and groups).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub i32);

/// How a caller names the chat to send to, before the protocol layer resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRef {
    /// The account's own Saved Messages.
    Me,
    /// A public username, stored without the leading `@`.
    Username(String),
    /// A marked chat id the account has already seen.
    Id(ChatId),
}

impl std::str::FromStr for ChatRef {
    type Err = Error;

    /// Parses `me`, `@name`, `name`, or a (possibly negative) integer id.
    fn from_str(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("me") {
            return Ok(Self::Me);
        }
        if let Ok(id) = s.parse::<i64>() {
            return Ok(Self::Id(ChatId(id)));
        }
        let name = s.strip_prefix('@').unwrap_or(s);
        let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            Ok(Self::Username(name.to_owned()))
        } else {
            Err(Error::InvalidChatRef(s.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_chat_ref_form() {
        assert_eq!("me".parse::<ChatRef>().unwrap(), ChatRef::Me);
        assert_eq!(
            "@Hai_01".parse::<ChatRef>().unwrap(),
            ChatRef::Username("Hai_01".into())
        );
        assert_eq!(
            "hai".parse::<ChatRef>().unwrap(),
            ChatRef::Username("hai".into())
        );
        assert_eq!(
            "-1001234".parse::<ChatRef>().unwrap(),
            ChatRef::Id(ChatId(-1_001_234))
        );
        assert!("@bad name".parse::<ChatRef>().is_err());
        assert!("@".parse::<ChatRef>().is_err());
    }

    #[test]
    fn channel_id_strips_the_mark_only_from_channels() {
        assert_eq!(ChatId(-1_001_234_567_890).channel_id(), Some(1_234_567_890));
        assert_eq!(ChatId(-5_000_000_001).channel_id(), None);
        assert_eq!(ChatId(123_456_789).channel_id(), None);
    }
}
