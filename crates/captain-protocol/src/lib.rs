//! Layer 1: converts between Telegram's MTProto protocol and `captain-core` types.
//!
//! This crate owns the grammers dependency, the session storage split between the Keychain and
//! disk, login, and the inbound and outbound conversions. It never parses Markdown or HTML; that
//! is `captain-format`. Callers never see a grammers type.

mod connection;
mod entities;
mod error;
mod handle;
mod inbound;
mod outbound;
mod session;

pub use connection::{Connection, LoginPrompt};
pub use error::{Error, Result};
pub use handle::Handle;
pub use session::{KeychainSession, SessionError};
