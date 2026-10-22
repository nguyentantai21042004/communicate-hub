//! Errors from the MTProto protocol layer, with grammers types mapped away.

/// Why a protocol operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The message broke a Telegram limit and was not sent.
    #[error(transparent)]
    Invalid(#[from] captain_core::Error),
    /// Credentials or session state could not be read or written.
    #[error(transparent)]
    Vault(#[from] captain_vault::Error),
    /// `captain auth init` has not stored API credentials for this profile yet.
    #[error("no API credentials for profile {0}; run `captain auth init` first")]
    NoApiCredentials(String),
    /// The session is not logged in to an account.
    #[error("profile {0} is not logged in; run `captain auth login` first")]
    NotAuthorized(String),
    /// The chat could not be resolved to something Telegram accepts.
    #[error(
        "chat {0} is unknown to this account; use @username, or receive a message from it first"
    )]
    UnknownChat(String),
    /// Login was refused, for example a wrong code or password.
    #[error("login failed: {0}")]
    Login(String),
    /// Telegram returned an RPC error, such as `FLOOD_WAIT_30` or `PEER_ID_INVALID`.
    #[error("telegram rejected {request}: {message}")]
    Rpc {
        /// Request that failed.
        request: &'static str,
        /// Error Telegram returned.
        message: String,
    },
    /// A local file to upload could not be read.
    #[error("cannot read {path}: {source}")]
    File {
        /// File involved.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A photo is too large to send as a photo.
    #[error(
        "{name} is {size_bytes} bytes; photos above {max_bytes} must be sent as original (document)"
    )]
    PhotoTooLarge {
        /// File name.
        name: String,
        /// File size.
        size_bytes: u64,
        /// Photo size limit.
        max_bytes: u64,
    },
    /// Telegram answered with something this layer does not understand.
    #[error("unexpected response to {request}: {detail}")]
    Unexpected {
        /// Request that got the response.
        request: &'static str,
        /// What was unexpected.
        detail: String,
    },
    /// Session state is unusable, for example a corrupt auth key.
    #[error("session state: {0}")]
    Session(String),
}

/// Result alias for protocol operations.
pub type Result<T> = std::result::Result<T, Error>;

/// What a grammers error string means for the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramFailure {
    /// The auth key or session can no longer be used.
    NotAuthorized,
    /// `FLOOD_WAIT` or `SLOWMODE_WAIT` for this many seconds.
    FloodWait(u64),
    /// Neither a dead session nor a flood wait.
    Other,
}

// Substrings of a grammers 0.10 error. `AUTH_KEY_` covers every `AUTH_KEY_*` name,
// including `AUTH_KEY_UNREGISTERED` and `AUTH_KEY_DUPLICATED`. `rpc error 401:` is any
// 401. The transport line is `-404` (auth key not found), not `-429`.
const NOT_AUTHORIZED: &[&str] = &[
    "rpc error 401:",
    "SESSION_REVOKED",
    "SESSION_EXPIRED",
    "AUTH_KEY_UNREGISTERED",
    "AUTH_KEY_DUPLICATED",
    "AUTH_KEY_",
    "bad status (negative length -404)",
];
const WAIT_NAMES: [&str; 2] = ["FLOOD_WAIT", "SLOWMODE_WAIT"];
const FLOOD_VALUE: &str = "(value:";

/// Classifies a grammers error string.
///
/// `message` is the display text of an invocation error, or the RPC or transport text
/// inside it. A dead session matches the auth-key, session, 401, and transport `-404`
/// markers. A flood wait is grammers 0.10's `FLOOD_WAIT (value: N)` or `SLOWMODE_WAIT (value: N)`.
pub fn classify_failure(message: &str) -> TelegramFailure {
    if NOT_AUTHORIZED.iter().any(|marker| message.contains(marker)) {
        return TelegramFailure::NotAuthorized;
    }
    if let Some(secs) = flood_wait_secs(message) {
        return TelegramFailure::FloodWait(secs);
    }
    TelegramFailure::Other
}

fn flood_wait_secs(message: &str) -> Option<u64> {
    let (wait_at, name_len) = WAIT_NAMES
        .into_iter()
        .filter_map(|name| message.find(name).map(|at| (at, name.len())))
        .min_by_key(|(at, _)| *at)?;
    let tail = &message[wait_at + name_len..];
    let value_at = tail.find(FLOOD_VALUE)?;
    let digits: String = tail[value_at + FLOOD_VALUE.len()..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

pub(crate) fn rpc(request: &'static str) -> impl FnOnce(grammers_client::InvocationError) -> Error {
    move |error| from_invocation("session", request, &error)
}

pub(crate) fn from_invocation(
    profile: &str,
    request: &'static str,
    error: &grammers_client::InvocationError,
) -> Error {
    // grammers prints every session failure as "session error" and drops the inner text.
    // The refusal is still the concrete [`crate::session::SessionError::AuthKeyConflict`].
    if auth_key_refusal(error)
        || matches!(
            classify_failure(&error.to_string()),
            TelegramFailure::NotAuthorized
        )
    {
        Error::NotAuthorized(profile.to_owned())
    } else {
        Error::Rpc {
            request,
            message: error.to_string(),
        }
    }
}

pub(crate) fn from_session_box(
    profile: &str,
    error: Box<dyn std::error::Error + Send + Sync>,
) -> Error {
    match error.downcast::<crate::session::SessionError>() {
        Ok(session) if session.is_auth_key_conflict() => Error::NotAuthorized(profile.to_owned()),
        Ok(session) => Error::Session(session.to_string()),
        Err(error) => Error::Session(error.to_string()),
    }
}

fn auth_key_refusal(error: &grammers_client::InvocationError) -> bool {
    match error {
        grammers_client::InvocationError::Session(inner) => inner
            .downcast_ref::<crate::session::SessionError>()
            .is_some_and(crate::session::SessionError::is_auth_key_conflict),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_revoked_sessions_and_flood_waits() {
        for message in [
            "401 SESSION_REVOKED",
            "rpc error 401: SESSION_REVOKED",
            "AUTH_KEY_UNREGISTERED",
            "406 AUTH_KEY_DUPLICATED",
            "rpc error 406: AUTH_KEY_DUPLICATED",
            "SESSION_EXPIRED",
            "rpc error 401: SESSION_EXPIRED",
            "bad status (negative length -404)",
            "request error: transport error: bad status (negative length -404)",
            "rpc error 401: USER_DEACTIVATED",
        ] {
            assert_eq!(
                classify_failure(message),
                TelegramFailure::NotAuthorized,
                "{message}"
            );
        }
        assert_eq!(
            classify_failure("rpc error 420: FLOOD_WAIT (value: 31)"),
            TelegramFailure::FloodWait(31)
        );
        assert_eq!(
            classify_failure(
                "request error: rpc error 420: FLOOD_WAIT caused by messages.getDifference (value: 31)"
            ),
            TelegramFailure::FloodWait(31)
        );
        assert_eq!(
            classify_failure("rpc error 420: SLOWMODE_WAIT (value: 12)"),
            TelegramFailure::FloodWait(12)
        );
        assert_eq!(
            classify_failure("rpc error 400: PEER_ID_INVALID"),
            TelegramFailure::Other
        );
        assert_eq!(
            classify_failure("rpc error 303: USER_MIGRATE (value: 2)"),
            TelegramFailure::Other
        );
        assert_eq!(
            classify_failure("bad status (negative length -429)"),
            TelegramFailure::Other
        );
    }

    #[test]
    fn auth_key_refusal_is_not_a_session_error_string() {
        use crate::session::SessionError;
        use grammers_client::InvocationError;

        let invocation = InvocationError::Session(Box::new(SessionError::AuthKeyConflict));
        assert_eq!(invocation.to_string(), "session error");
        let mapped = from_invocation("default", "users.getUsers", &invocation);
        assert!(
            matches!(mapped, Error::NotAuthorized(ref profile) if profile == "default"),
            "{mapped}"
        );
        assert!(
            mapped.to_string().contains("captain auth login"),
            "{mapped}"
        );
        assert!(!mapped.to_string().contains("session error"), "{mapped}");

        let stored = from_session_box("default", Box::new(SessionError::AuthKeyConflict));
        assert!(matches!(stored, Error::NotAuthorized(_)), "{stored}");

        let other = InvocationError::Session(Box::new(SessionError::Storage("disk full".into())));
        let mapped = from_invocation("default", "users.getUsers", &other);
        assert!(
            matches!(mapped, Error::Rpc { ref message, .. } if message == "session error"),
            "{mapped}"
        );
    }
}
