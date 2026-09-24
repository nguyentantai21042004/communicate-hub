//! Ticket vocabulary: where a ticket goes, what state it is in, and how it was labelled.

use captain_core::{ChatId, MessageId};
use serde::{Deserialize, Serialize};

/// Declares a string-backed enum with `as_str` and `FromStr`, for storage and the CLI.
macro_rules! str_enum {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal,)+ }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// Stable name, as stored and printed.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> std::result::Result<Self, String> {
                match s {
                    $($text => Ok(Self::$variant),)+
                    other => Err(format!("unknown {} {other:?}", stringify!($name))),
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
pub(crate) use str_enum;

str_enum! {
    /// Where a triaged ticket is headed.
    pub enum Route {
        /// A draft reply, written by the runner in P2.
        Draft => "draft",
        /// The periodic digest, for low-confidence or stale tickets.
        Digest => "digest",
    }
}

str_enum! {
    /// Lifecycle state of a ticket.
    pub enum TicketState {
        /// Not worth any action; `reason` says why (paused, self-handled, ...).
        Skipped => "skipped",
        /// Waiting for the digest.
        Digest => "digest",
        /// Waiting for a draft. In P1 it waits for its implicit label instead.
        Queued => "queued",
        /// Labelled from what the owner did afterwards.
        Resolved => "resolved",
    }
}

str_enum! {
    /// Label derived from the owner's behaviour after the ticket's anchor.
    pub enum ImplicitLabel {
        /// Replied in the same chat and topic.
        Answered => "answered",
        /// Did not reply there, but wrote to the asker directly.
        AnsweredElsewhere => "answered_elsewhere",
        /// Read the chat past the anchor without replying.
        ReadOnly => "read_only",
        /// Neither read nor replied.
        Unseen => "unseen",
    }
}

str_enum! {
    /// Label the owner gives a ticket in review.
    pub enum ExplicitLabel {
        /// A draft would have helped.
        Want => "want",
        /// Should not have been a ticket.
        Noise => "noise",
        /// No opinion; excluded from precision.
        Skip => "skip",
    }
}

impl ImplicitLabel {
    /// Whether the label counts as the owner acting on the ticket.
    pub fn is_answer(self) -> bool {
        matches!(self, Self::Answered | Self::AnsweredElsewhere)
    }
}

/// A chat, narrowed to one forum topic when it has topics. One burst and one draft slot exist
/// per key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadKey {
    /// Marked chat id.
    pub chat: ChatId,
    /// Forum topic, `None` outside forums and in the General topic.
    pub topic: Option<MessageId>,
}

/// What the owner did within the resolve window after a ticket's anchor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveFacts {
    /// Sent a message on the same key.
    pub replied_on_key: bool,
    /// Sent a message to the asker's private chat.
    pub replied_in_dm: bool,
    /// Read the chat up to or past the anchor.
    pub read_past_anchor: bool,
}

impl ResolveFacts {
    /// The implicit label these facts give, most active behaviour first.
    pub fn label(self) -> ImplicitLabel {
        if self.replied_on_key {
            ImplicitLabel::Answered
        } else if self.replied_in_dm {
            ImplicitLabel::AnsweredElsewhere
        } else if self.read_past_anchor {
            ImplicitLabel::ReadOnly
        } else {
            ImplicitLabel::Unseen
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strongest_behaviour_wins() {
        let all = ResolveFacts {
            replied_on_key: true,
            replied_in_dm: true,
            read_past_anchor: true,
        };
        assert_eq!(all.label(), ImplicitLabel::Answered);
        let dm = ResolveFacts {
            replied_on_key: false,
            ..all
        };
        assert_eq!(dm.label(), ImplicitLabel::AnsweredElsewhere);
        assert_eq!(ResolveFacts::default().label(), ImplicitLabel::Unseen);
    }

    #[test]
    fn names_round_trip() {
        for state in TicketState::ALL {
            assert_eq!(state.as_str().parse::<TicketState>().unwrap(), *state);
        }
        assert!("nope".parse::<Route>().is_err());
    }
}
