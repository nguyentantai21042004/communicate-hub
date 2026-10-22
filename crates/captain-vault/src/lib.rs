//! Credential and local-state storage for telegram-captain.
//!
//! Secrets (API hash, MTProto auth keys) live in the macOS Keychain through [`KeychainStore`].
//! Non-secret state lives in a per-profile directory written with owner-only permissions.
//! This crate knows nothing about MTProto: callers decide what each secret means.

mod error;
mod fs;
mod profile;
mod secret;
mod store;
mod vault;

pub use error::{Error, Result};
pub use fs::write_private;
pub use profile::Profile;
pub use secret::Secret;
pub use store::{
    HUB_BOT_TOKEN_ACCOUNT, HUB_KEYCHAIN_SERVICE, KeychainStore, MemoryStore, SecretStore,
};
pub use vault::{ApiCredentials, Vault};
