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

pub(crate) fn rpc(request: &'static str) -> impl FnOnce(grammers_client::InvocationError) -> Error {
    move |e| Error::Rpc {
        request,
        message: e.to_string(),
    }
}
