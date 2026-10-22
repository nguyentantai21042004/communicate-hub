//! Networks, conversation ids, and the Telegram ids a topic sink returns.
//!
//! A conversation id is the string `network:kind:external_id`. The external id may contain
//! colons. Parsing is case sensitive and rejects an empty or missing part.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const TELEGRAM_NAME: &str = "telegram"; // wire name for Network::Telegram
const ZALO_NAME: &str = "zalo"; // wire name for Network::Zalo
const MESSENGER_NAME: &str = "messenger"; // wire name for Network::Messenger
const DM_NAME: &str = "dm"; // wire name for ConversationKind::Dm
const GROUP_NAME: &str = "group"; // wire name for ConversationKind::Group
const ID_SEPARATOR: char = ':'; // separates network, kind, and external id

/// Source or destination network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// Telegram user or bot chats.
    Telegram,
    /// Zalo, reached through the Node sidecar.
    Zalo,
    /// Facebook Messenger.
    Messenger,
}

impl Network {
    /// Wire name: `telegram`, `zalo`, or `messenger`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => TELEGRAM_NAME,
            Self::Zalo => ZALO_NAME,
            Self::Messenger => MESSENGER_NAME,
        }
    }
}

impl std::fmt::Display for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Network {
    type Err = Error;

    /// Parses `telegram`, `zalo`, or `messenger`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownNetwork`] for any other string, including a different case.
    fn from_str(value: &str) -> Result<Self> {
        match value {
            TELEGRAM_NAME => Ok(Self::Telegram),
            ZALO_NAME => Ok(Self::Zalo),
            MESSENGER_NAME => Ok(Self::Messenger),
            _ => Err(Error::UnknownNetwork {
                value: value.to_owned(),
            }),
        }
    }
}

/// Whether a conversation is a direct message or a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationKind {
    /// One-to-one chat.
    Dm,
    /// Group chat.
    Group,
}

impl ConversationKind {
    /// Wire name: `dm` or `group`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dm => DM_NAME,
            Self::Group => GROUP_NAME,
        }
    }
}

impl std::fmt::Display for ConversationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ConversationKind {
    type Err = Error;

    /// Parses `dm` or `group`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownKind`] for any other string, including a different case.
    fn from_str(value: &str) -> Result<Self> {
        match value {
            DM_NAME => Ok(Self::Dm),
            GROUP_NAME => Ok(Self::Group),
            _ => Err(Error::UnknownKind {
                value: value.to_owned(),
            }),
        }
    }
}

/// Identity of one conversation on one network.
///
/// [`Display`](std::fmt::Display) and [`FromStr`](std::str::FromStr) use
/// `network:kind:external_id`. JSON is that same string, not an object.
/// Fields are public so callers can build an id without parsing, but an empty `external_id`
/// does not round-trip through the string form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ConversationId {
    /// Network that owns the conversation.
    pub network: Network,
    /// Direct message or group.
    pub kind: ConversationKind,
    /// Id assigned by the network. May contain colons.
    pub external_id: String,
}

impl std::fmt::Display for ConversationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{ID_SEPARATOR}{}{ID_SEPARATOR}{}",
            self.network.as_str(),
            self.kind.as_str(),
            self.external_id,
        )
    }
}

impl std::str::FromStr for ConversationId {
    type Err = Error;

    /// Parses `network:kind:external_id`.
    ///
    /// The external id is everything after the second separator, so it may contain colons.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedConversationId`] when a part is missing or empty.
    /// Returns [`Error::UnknownNetwork`] or [`Error::UnknownKind`] when those parts are present
    /// but not recognized.
    fn from_str(value: &str) -> Result<Self> {
        let Some((network, rest)) = value.split_once(ID_SEPARATOR) else {
            return Err(Error::MalformedConversationId {
                value: value.to_owned(),
            });
        };
        let Some((kind, external_id)) = rest.split_once(ID_SEPARATOR) else {
            return Err(Error::MalformedConversationId {
                value: value.to_owned(),
            });
        };
        if network.is_empty() || kind.is_empty() || external_id.is_empty() {
            return Err(Error::MalformedConversationId {
                value: value.to_owned(),
            });
        }
        Ok(Self {
            network: network.parse()?,
            kind: kind.parse()?,
            external_id: external_id.to_owned(),
        })
    }
}

impl From<ConversationId> for String {
    fn from(id: ConversationId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for ConversationId {
    type Error = Error;

    /// Parses the canonical id string.
    ///
    /// # Errors
    ///
    /// Same as [`FromStr`](std::str::FromStr).
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

/// One message inside a [`ConversationId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageRef {
    /// Conversation that holds the message.
    pub conversation: ConversationId,
    /// Message id assigned by the network, as a string.
    pub message_id: String,
}

/// Forum topic in the bridge supergroup.
///
/// TELEGRAM: this is the Bot API `message_thread_id`, the message id of the topic's first post.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TopicId(pub i32);

/// Message id inside the bridge supergroup.
///
/// TELEGRAM: this is the Bot API `message_id` of a post in the supergroup, not a source-network id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TgMessageId(pub i32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_id_round_trips_every_network_and_kind() -> Result<()> {
        let samples = ["telegram:dm:42", "zalo:group:a:b:c", "messenger:dm:psid 1"];
        for sample in samples {
            let id: ConversationId = sample.parse()?;
            assert_eq!(id.to_string(), sample);
            assert_eq!(sample.parse::<ConversationId>()?, id);
        }
        Ok(())
    }

    #[test]
    fn conversation_id_rejects_malformed_strings() {
        let malformed = ["", "zalo", "zalo:dm", "zalo:dm:", ":dm:1", "zalo::1", ":"];
        for sample in malformed {
            assert!(
                matches!(
                    sample.parse::<ConversationId>(),
                    Err(Error::MalformedConversationId { .. })
                ),
                "{sample}"
            );
        }
        assert!(matches!(
            "nope:dm:1".parse::<ConversationId>(),
            Err(Error::UnknownNetwork { value }) if value == "nope"
        ));
        assert!(matches!(
            "Zalo:dm:1".parse::<ConversationId>(),
            Err(Error::UnknownNetwork { value }) if value == "Zalo"
        ));
        assert!(matches!(
            "zalo:channel:1".parse::<ConversationId>(),
            Err(Error::UnknownKind { value }) if value == "channel"
        ));
        assert!(matches!(
            "zalo:DM:1".parse::<ConversationId>(),
            Err(Error::UnknownKind { value }) if value == "DM"
        ));
    }

    #[test]
    fn empty_external_id_does_not_round_trip() {
        let id = ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Dm,
            external_id: String::new(),
        };
        assert!(id.to_string().parse::<ConversationId>().is_err());
    }

    #[test]
    fn network_from_str_accepts_only_wire_names() -> Result<()> {
        assert_eq!("telegram".parse::<Network>()?, Network::Telegram);
        assert_eq!("zalo".parse::<Network>()?, Network::Zalo);
        assert_eq!("messenger".parse::<Network>()?, Network::Messenger);
        assert_eq!(Network::Zalo.as_str(), "zalo");
        assert_eq!(Network::Zalo.to_string(), "zalo");
        assert!("Telegram".parse::<Network>().is_err());
        assert!("".parse::<Network>().is_err());
        assert!("signal".parse::<Network>().is_err());
        Ok(())
    }

    #[test]
    fn conversation_id_json_is_the_canonical_string() -> serde_json::Result<()> {
        let id = ConversationId {
            network: Network::Messenger,
            kind: ConversationKind::Group,
            external_id: "thread:1".into(),
        };
        let json = serde_json::to_string(&id)?;
        assert_eq!(json, "\"messenger:group:thread:1\"");
        let parsed: ConversationId = serde_json::from_str(&json)?;
        assert_eq!(parsed, id);
        assert!(serde_json::from_str::<ConversationId>("\"zalo:dm:\"").is_err());
        Ok(())
    }

    #[test]
    fn network_and_kind_json_are_lowercase() -> serde_json::Result<()> {
        assert_eq!(serde_json::to_string(&Network::Telegram)?, "\"telegram\"");
        assert_eq!(serde_json::to_string(&ConversationKind::Dm)?, "\"dm\"");
        assert_eq!(
            serde_json::to_string(&ConversationKind::Group)?,
            "\"group\""
        );
        Ok(())
    }
}
