//! Typed access to one profile's secrets and state directory.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Error, Profile, Result, Secret, SecretStore};

const API_ENTRY: &str = "api";
const APP_DIR_NAME: &str = "telegram-captain";

/// Telegram application credentials from <https://my.telegram.org/apps>.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCredentials {
    /// Public application id.
    pub api_id: i32,
    /// Application hash. Not account access on its own, but still kept secret.
    pub api_hash: Secret,
}

#[derive(Serialize, Deserialize)]
struct StoredApi {
    api_id: i32,
    api_hash: String,
}

/// One profile's view of the secret store, plus where its non-secret state lives.
#[derive(Clone)]
pub struct Vault {
    store: Arc<dyn SecretStore>,
    profile: Profile,
    state_dir: PathBuf,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("profile", &self.profile)
            .field("state_dir", &self.state_dir)
            .finish()
    }
}

impl Vault {
    /// Vault for `profile`, with state under the platform data directory.
    ///
    /// # Errors
    ///
    /// [`Error::NoHomeDir`] when the home directory cannot be determined.
    pub fn open(store: Arc<dyn SecretStore>, profile: Profile) -> Result<Self> {
        let state_dir = data_dir()?.join(APP_DIR_NAME).join(profile.as_str());
        Ok(Self {
            store,
            profile,
            state_dir,
        })
    }

    /// Vault with an explicit state directory, for tests and custom layouts.
    pub fn with_state_dir(
        store: Arc<dyn SecretStore>,
        profile: Profile,
        state_dir: PathBuf,
    ) -> Self {
        Self {
            store,
            profile,
            state_dir,
        }
    }

    /// The profile this vault serves.
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Directory for this profile's non-secret state. It may not exist yet.
    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    /// Reads the stored API credentials, or `None` before `captain auth init`.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] on backend failure and [`Error::Corrupt`] if the entry cannot be
    /// decoded.
    pub fn api_credentials(&self) -> Result<Option<ApiCredentials>> {
        let Some(raw) = self.secret(API_ENTRY)? else {
            return Ok(None);
        };
        let stored: StoredApi = serde_json::from_str(raw.expose()).map_err(|e| Error::Corrupt {
            entry: API_ENTRY.to_owned(),
            reason: e.to_string(),
        })?;
        Ok(Some(ApiCredentials {
            api_id: stored.api_id,
            api_hash: Secret::new(stored.api_hash),
        }))
    }

    /// Stores API credentials, replacing previous ones.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the write is refused.
    pub fn set_api_credentials(&self, creds: &ApiCredentials) -> Result<()> {
        let stored = StoredApi {
            api_id: creds.api_id,
            api_hash: creds.api_hash.expose().to_owned(),
        };
        let json = serde_json::to_string(&stored).map_err(|e| Error::Corrupt {
            entry: API_ENTRY.to_owned(),
            reason: e.to_string(),
        })?;
        self.set_secret(API_ENTRY, &Secret::new(json))
    }

    /// Reads a named secret of this profile.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] on backend failure.
    pub fn secret(&self, name: &str) -> Result<Option<Secret>> {
        self.store.get(&self.entry(name))
    }

    /// Creates or replaces a named secret of this profile.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the write is refused.
    pub fn set_secret(&self, name: &str, value: &Secret) -> Result<()> {
        self.store.set(&self.entry(name), value)
    }

    /// Deletes a named secret of this profile. Missing secrets are ignored.
    ///
    /// # Errors
    ///
    /// [`Error::Keychain`] when the delete is refused.
    pub fn delete_secret(&self, name: &str) -> Result<()> {
        self.store.delete(&self.entry(name))
    }

    fn entry(&self, name: &str) -> String {
        format!("{}/{name}", self.profile)
    }
}

/// Platform data directory: `~/Library/Application Support` on macOS, `$XDG_DATA_HOME` or
/// `~/.local/share` elsewhere.
fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(Error::NoHomeDir)?;
    if cfg!(target_os = "macos") {
        return Ok(home.join("Library/Application Support"));
    }
    Ok(std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    #[test]
    fn api_credentials_round_trip_under_profile_prefix() {
        let store = Arc::new(MemoryStore::default());
        let vault = Vault::with_state_dir(
            store.clone(),
            Profile::new("work").unwrap(),
            "/tmp/x".into(),
        );
        assert_eq!(vault.api_credentials().unwrap(), None);

        let creds = ApiCredentials {
            api_id: 42,
            api_hash: Secret::new("abc"),
        };
        vault.set_api_credentials(&creds).unwrap();
        assert_eq!(vault.api_credentials().unwrap(), Some(creds));
        assert_eq!(store.entries(), vec!["work/api".to_owned()]);
    }

    #[test]
    fn corrupt_entry_is_reported_not_panicked() {
        let store = Arc::new(MemoryStore::default());
        let vault = Vault::with_state_dir(store, Profile::default(), "/tmp/x".into());
        vault
            .set_secret(API_ENTRY, &Secret::new("{not json"))
            .unwrap();
        assert!(matches!(
            vault.api_credentials(),
            Err(Error::Corrupt { .. })
        ));
    }
}
