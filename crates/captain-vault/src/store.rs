//! Backends that hold secrets: the OS keychain, and an in-memory map for tests.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{Error, Result, Secret};

/// Key-value storage for secrets, addressed by an entry name.
pub trait SecretStore: Send + Sync {
    /// Reads an entry. Returns `Ok(None)` when it does not exist.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the backend fails, for example when the user denies access.
    fn get(&self, entry: &str) -> Result<Option<Secret>>;

    /// Creates or replaces an entry.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the backend refuses the write.
    fn set(&self, entry: &str, value: &Secret) -> Result<()>;

    /// Deletes an entry. Deleting a missing entry succeeds.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the backend refuses the delete.
    fn delete(&self, entry: &str) -> Result<()>;
}

/// Keychain service every Captain entry is filed under, visible in Keychain Access.
pub const KEYCHAIN_SERVICE: &str = "dev.tantai.telegram-captain";

/// Keychain service for the bridge bot token. It is not [`KEYCHAIN_SERVICE`].
pub const HUB_KEYCHAIN_SERVICE: &str = "dev.tantai.comms-hub";

/// Account name of the bot token inside [`HUB_KEYCHAIN_SERVICE`].
///
/// Callers prefix it with the profile, for example `default/bot-token`.
pub const HUB_BOT_TOKEN_ACCOUNT: &str = "bot-token";

/// Secret store backed by the macOS Keychain (or the platform keyring elsewhere).
///
/// SECURITY: macOS ties Keychain access to the binary's code signature. An unsigned debug build
/// changes signature on every rebuild, so macOS asks again for permission; answer
/// "Always Allow" only for binaries you built yourself.
#[derive(Debug, Clone)]
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// Store using [`KEYCHAIN_SERVICE`].
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_owned(),
        }
    }

    /// Store for the bridge bot token, using [`HUB_KEYCHAIN_SERVICE`].
    ///
    /// The account name is [`HUB_BOT_TOKEN_ACCOUNT`]. This is a different Keychain item from
    /// the MTProto session.
    pub fn hub_bot() -> Self {
        Self {
            service: HUB_KEYCHAIN_SERVICE.to_owned(),
        }
    }

    /// Keychain service this store writes.
    pub fn service_name(&self) -> &str {
        &self.service
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name).map_err(|e| keychain_err("open", name, &e))
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, entry: &str) -> Result<Option<Secret>> {
        match self.entry(entry)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_err("get", entry, &e)),
        }
    }

    fn set(&self, entry: &str, value: &Secret) -> Result<()> {
        self.entry(entry)?
            .set_password(value.expose())
            .map_err(|e| keychain_err("set", entry, &e))
    }

    fn delete(&self, entry: &str) -> Result<()> {
        match self.entry(entry)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_err("delete", entry, &e)),
        }
    }
}

fn keychain_err(op: &'static str, entry: &str, e: &keyring::Error) -> Error {
    Error::Keychain {
        op,
        entry: entry.to_owned(),
        message: e.to_string(),
    }
}

/// In-memory secret store for tests and dry runs. Contents vanish with the process.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<String, Secret>>);

impl MemoryStore {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Secret>> {
        // A panic while holding this lock cannot leave the map half-written, so recover it.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Names of the entries currently stored, sorted.
    pub fn entries(&self) -> Vec<String> {
        let mut names: Vec<_> = self.map().keys().cloned().collect();
        names.sort();
        names
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, entry: &str) -> Result<Option<Secret>> {
        Ok(self.map().get(entry).cloned())
    }

    fn set(&self, entry: &str, value: &Secret) -> Result<()> {
        self.map().insert(entry.to_owned(), value.clone());
        Ok(())
    }

    fn delete(&self, entry: &str) -> Result<()> {
        self.map().remove(entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_bot_service_is_separate_from_the_session() {
        assert_ne!(HUB_KEYCHAIN_SERVICE, KEYCHAIN_SERVICE);
        assert_eq!(
            KeychainStore::hub_bot().service_name(),
            HUB_KEYCHAIN_SERVICE
        );
        assert_eq!(KeychainStore::new().service_name(), KEYCHAIN_SERVICE);
        assert_eq!(HUB_BOT_TOKEN_ACCOUNT, "bot-token");
    }
}
