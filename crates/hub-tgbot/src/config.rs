//! Caller-supplied bot settings.
//!
//! This crate never reads the environment or the filesystem for the token.
//! The caller passes [`BotToken`] in. [`BotConfig::api_base_url`] is the origin
//! only, for example `https://api.telegram.org` or `http://127.0.0.1:9`.
//! The client appends `/bot` and the token itself.

use std::fmt;

const REDACTED: &str = "<redacted>";

/// Bot token whose `Debug` and `Display` output is `<redacted>`.
///
/// SECURITY: logs, panic messages, and error strings must not show the token.
/// The value is readable only inside this crate.
#[derive(Clone, PartialEq, Eq)]
pub struct BotToken(String);

impl BotToken {
    /// Wraps a bot token.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Settings for one bridge supergroup.
///
/// `api_base_url` is the origin only. [`crate::BotClient::new`] rejects an empty origin
/// and an origin that already contains the token.
#[derive(Clone, PartialEq, Eq)]
pub struct BotConfig {
    /// Bot token supplied by the caller.
    pub token: BotToken,
    /// Origin of the Bot API, without `/bot` and without the token.
    pub api_base_url: String,
    /// Forum supergroup the bridge writes into. Supergroup ids are negative.
    pub group_id: i64,
}

impl fmt::Debug for BotConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotConfig")
            .field("token", &self.token)
            .field("api_base_url", &self.api_base_url)
            .field("group_id", &self.group_id)
            .finish()
    }
}
