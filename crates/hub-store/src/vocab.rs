//! Store enums that `hub-core` does not own, plus parsers for the types it does.
//!
//! `Network`, `ConversationKind`, `ConversationId`, `ModuleHealth`, `ModuleState` and
//! `RemoteEvent` come from `hub-core`. Direction, content kind and log level are counter and
//! log columns, so they stay here.

use hub_core::{ConversationKind, ModuleState, Network};

use crate::{Error, Result};

macro_rules! stored_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$vmeta:meta])* $var:ident => $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $var,)+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$var,)+];

            /// Stable name stored in `SQLite` and emitted in JSON.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$var => $text,)+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> std::result::Result<Self, String> {
                match s {
                    $($text => Ok(Self::$var),)+
                    other => Err(format!("unknown variant {other}")),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

stored_enum! {
    /// Whether the account sent the message or received it.
    pub enum Direction {
        /// Someone else sent it.
        In => "in",
        /// The account sent it.
        Out => "out",
    }
}

stored_enum! {
    /// What the message carried. The store keeps this tag and drops the bytes.
    pub enum ContentKind {
        /// Plain text.
        Text => "text",
        /// A photo.
        Photo => "photo",
        /// A video.
        Video => "video",
        /// A voice note.
        Voice => "voice",
        /// A file.
        File => "file",
        /// A sticker.
        Sticker => "sticker",
        /// A poll.
        Poll => "poll",
        /// A bank card image the bridge recognised.
        BankCard => "bank_card",
        /// Anything else, including audio, GIF, location and contact.
        Other => "other",
    }
}

stored_enum! {
    /// Severity of a bridge or module log row.
    pub enum LogLevel {
        /// Diagnostic detail.
        Debug => "debug",
        /// Normal progress.
        Info => "info",
        /// A degraded path that still finished.
        Warn => "warn",
        /// A failure.
        Error => "error",
    }
}

const MODULE_OK: &str = "ok"; // ModuleState::Ok
const MODULE_DEGRADED: &str = "degraded"; // ModuleState::Degraded
const MODULE_DOWN: &str = "down"; // ModuleState::Down
const MODULE_UNKNOWN: &str = "unknown"; // ModuleState::Unknown

pub(crate) fn parse<T: std::str::FromStr<Err = String>>(value: &str) -> Result<T> {
    value.parse().map_err(Error::Corrupt)
}

pub(crate) fn parse_network(value: &str) -> Result<Network> {
    value
        .parse()
        .map_err(|err: hub_core::Error| Error::Corrupt(err.to_string()))
}

pub(crate) fn parse_kind(value: &str) -> Result<ConversationKind> {
    value
        .parse()
        .map_err(|err: hub_core::Error| Error::Corrupt(err.to_string()))
}

pub(crate) fn module_state_name(state: ModuleState) -> &'static str {
    match state {
        ModuleState::Ok => MODULE_OK,
        ModuleState::Degraded => MODULE_DEGRADED,
        ModuleState::Down => MODULE_DOWN,
        ModuleState::Unknown => MODULE_UNKNOWN,
    }
}

pub(crate) fn parse_module_state(value: &str) -> Result<ModuleState> {
    match value {
        MODULE_OK => Ok(ModuleState::Ok),
        MODULE_DEGRADED => Ok(ModuleState::Degraded),
        MODULE_DOWN => Ok(ModuleState::Down),
        MODULE_UNKNOWN => Ok(ModuleState::Unknown),
        other => Err(Error::Corrupt(format!("module state {other}"))),
    }
}

pub(crate) fn require_token(value: &str, what: &str) -> Result<()> {
    if value.is_empty() {
        Err(Error::Empty(what.to_owned()))
    } else {
        Ok(())
    }
}
