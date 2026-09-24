//! MTProto session storage split by sensitivity.
//!
//! SECURITY: the permanent auth keys are the account. Anyone holding one can act as the user
//! without a password or code. They live only in the Keychain. Everything else the session
//! tracks (datacenter addresses, the peer cache, the update state) is written to a `0600` JSON
//! file in the profile's state directory, so leaking that file does not leak the account.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use captain_vault::{Secret, Vault, write_private};
use grammers_session::types::{
    ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState,
};
use grammers_session::{Session, SessionData};
use serde::{Deserialize, Serialize};

const AUTH_ENTRY: &str = "mtproto_auth";
const STATE_FILE: &str = "mtproto_state.json";
const STATE_VERSION: u32 = 1;
const AUTH_KEY_BYTES: usize = 256;

/// grammers [`Session`] that keeps auth keys in the Keychain and the rest on disk.
///
/// Auth key and home datacenter changes are written through immediately; they are rare.
/// Peer cache and update state changes only mark the session dirty, and [`Self::flush`] writes
/// them. The connection flushes periodically and on shutdown.
pub struct KeychainSession {
    data: Mutex<SessionData>,
    vault: Vault,
    state_path: PathBuf,
    dirty: AtomicBool,
}

/// Error raised by [`KeychainSession`] through the grammers [`Session`] trait.
#[derive(Debug, thiserror::Error)]
#[error("session storage: {0}")]
pub struct SessionError(String);

#[derive(Serialize, Deserialize)]
struct StoredAuth {
    home_dc: i32,
    auth_keys: HashMap<i32, String>, // dc id -> hex-encoded 256-byte key
}

#[derive(Serialize, Deserialize)]
struct StoredState {
    version: u32,
    dc_options: Vec<DcOption>, // always written with `auth_key: None`
    peers: Vec<PeerInfo>,
    updates: UpdatesState,
}

impl KeychainSession {
    /// Loads the session for the vault's profile, or starts an empty one.
    ///
    /// # Errors
    ///
    /// [`SessionError`] when the Keychain entry or state file exists but cannot be decoded.
    pub fn load(vault: Vault) -> Result<Self, SessionError> {
        let state_path = vault.state_dir().join(STATE_FILE);
        let mut data = SessionData::default();

        if let Some(bytes) = read_optional(&state_path)? {
            let stored: StoredState = serde_json::from_slice(&bytes).map_err(err)?;
            for dc in stored.dc_options {
                data.dc_options.insert(
                    dc.id,
                    DcOption {
                        auth_key: None,
                        ..dc
                    },
                );
            }
            data.peer_infos = stored.peers.into_iter().map(|p| (p.id(), p)).collect();
            data.updates_state = stored.updates;
        }
        if let Some(secret) = vault.secret(AUTH_ENTRY).map_err(err)? {
            let auth: StoredAuth = serde_json::from_str(secret.expose()).map_err(err)?;
            data.home_dc = auth.home_dc;
            for (dc_id, hex) in auth.auth_keys {
                let key = decode_key(&hex)?;
                if let Some(dc) = data.dc_options.get_mut(&dc_id) {
                    dc.auth_key = Some(key);
                }
            }
        }
        Ok(Self {
            data: Mutex::new(data),
            vault,
            state_path,
            dirty: AtomicBool::new(false),
        })
    }

    /// Whether an auth key exists for the home datacenter. It does not prove the key is still
    /// bound to a logged-in user; ask the server for that.
    pub fn has_home_auth_key(&self) -> bool {
        let data = self.lock();
        data.dc_options
            .get(&data.home_dc)
            .is_some_and(|dc| dc.auth_key.is_some())
    }

    /// Writes pending peer cache and update state changes to disk.
    ///
    /// # Errors
    ///
    /// [`SessionError`] when the state file cannot be written. The session stays dirty, so the
    /// next flush retries.
    pub fn flush(&self) -> Result<(), SessionError> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        self.write_state()
            .inspect_err(|_| self.dirty.store(true, Ordering::Release))
    }

    /// Deletes the auth keys and the state file, returning the profile to logged-out.
    ///
    /// # Errors
    ///
    /// [`SessionError`] when the Keychain entry or the file cannot be removed.
    pub fn wipe(&self) -> Result<(), SessionError> {
        *self.lock() = SessionData::default();
        self.vault.delete_secret(AUTH_ENTRY).map_err(err)?;
        match std::fs::remove_file(&self.state_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(err(e)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SessionData> {
        // Every critical section below is a single field update; a panic cannot leave the data
        // half-written, so a poisoned lock is still safe to use.
        self.data.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_auth(&self) -> Result<(), SessionError> {
        let stored = {
            let data = self.lock();
            StoredAuth {
                home_dc: data.home_dc,
                auth_keys: data
                    .dc_options
                    .values()
                    .filter_map(|dc| dc.auth_key.map(|k| (dc.id, encode_hex(&k))))
                    .collect(),
            }
        };
        let json = serde_json::to_string(&stored).map_err(err)?;
        self.vault
            .set_secret(AUTH_ENTRY, &Secret::new(json))
            .map_err(err)
    }

    fn write_state(&self) -> Result<(), SessionError> {
        let stored = {
            let data = self.lock();
            StoredState {
                version: STATE_VERSION,
                dc_options: data
                    .dc_options
                    .values()
                    .map(|dc| DcOption {
                        auth_key: None,
                        ..dc.clone()
                    })
                    .collect(),
                peers: data.peer_infos.values().cloned().collect(),
                updates: data.updates_state.clone(),
            }
        };
        let json = serde_json::to_vec(&stored).map_err(err)?;
        write_private(&self.state_path, &json).map_err(err)
    }
}

impl Session for KeychainSession {
    type Error = SessionError;

    fn home_dc_id(&self) -> Result<i32, SessionError> {
        Ok(self.lock().home_dc)
    }

    fn set_home_dc_id(
        &self,
        dc_id: i32,
    ) -> grammers_session::BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(async move {
            self.lock().home_dc = dc_id;
            self.write_auth()
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, SessionError> {
        Ok(self.lock().dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(
        &self,
        dc_option: &DcOption,
    ) -> grammers_session::BoxFuture<'_, Result<(), SessionError>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let key_changed = {
                let mut data = self.lock();
                let old_key = data
                    .dc_options
                    .get(&dc_option.id)
                    .and_then(|dc| dc.auth_key);
                let changed = old_key != dc_option.auth_key;
                data.dc_options.insert(dc_option.id, dc_option);
                changed
            };
            if key_changed {
                self.write_auth()?;
            }
            self.write_state()
        })
    }

    fn peer(
        &self,
        peer: PeerId,
    ) -> grammers_session::BoxFuture<'_, Result<Option<PeerInfo>, SessionError>> {
        Box::pin(async move { Ok(self.lock().peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(
        &self,
        peer: &PeerInfo,
    ) -> grammers_session::BoxFuture<'_, Result<(), SessionError>> {
        let peer = peer.clone();
        Box::pin(async move {
            let changed = self
                .lock()
                .peer_infos
                .entry(peer.id())
                .or_insert_with(|| peer.clone())
                .extend_info(&peer);
            if changed {
                self.dirty.store(true, Ordering::Release);
            }
            Ok(())
        })
    }

    fn updates_state(&self) -> grammers_session::BoxFuture<'_, Result<UpdatesState, SessionError>> {
        Box::pin(async move { Ok(self.lock().updates_state.clone()) })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> grammers_session::BoxFuture<'_, Result<(), SessionError>> {
        Box::pin(async move {
            {
                let mut data = self.lock();
                let state = &mut data.updates_state;
                match update {
                    UpdateState::All(all) => *state = all,
                    UpdateState::Primary { pts, date, seq } => {
                        state.pts = pts;
                        state.date = date;
                        state.seq = seq;
                    }
                    UpdateState::Secondary { qts } => state.qts = qts,
                    UpdateState::Channel { id, pts } => {
                        state.channels.retain(|c| c.id != id);
                        state.channels.push(ChannelState { id, pts });
                    }
                }
            }
            self.dirty.store(true, Ordering::Release);
            Ok(())
        })
    }
}

fn err(e: impl std::fmt::Display) -> SessionError {
    SessionError(e.to_string())
}

fn read_optional(path: &std::path::Path) -> Result<Option<Vec<u8>>, SessionError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(err(e)),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}"); // writing to a String cannot fail
            s
        })
}

fn decode_key(hex: &str) -> Result<[u8; AUTH_KEY_BYTES], SessionError> {
    let bad = || {
        SessionError(format!(
            "auth key is not {AUTH_KEY_BYTES} hex-encoded bytes"
        ))
    };
    if hex.len() != AUTH_KEY_BYTES * 2 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut key = [0u8; AUTH_KEY_BYTES];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use captain_vault::{MemoryStore, Profile};

    use super::*;

    fn vault(store: Arc<MemoryStore>, dir: &std::path::Path) -> Vault {
        Vault::with_state_dir(store, Profile::default(), dir.to_owned())
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("captain-session-{tag}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn auth_key_goes_to_keychain_and_never_to_disk() {
        let dir = temp_dir("split");
        let store = Arc::new(MemoryStore::default());
        let session = KeychainSession::load(vault(store.clone(), &dir)).unwrap();
        assert!(!session.has_home_auth_key());

        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([7u8; AUTH_KEY_BYTES]);
        session.set_dc_option(&dc).await.unwrap();
        session.set_home_dc_id(2).await.unwrap();

        assert_eq!(store.entries(), vec![format!("default/{AUTH_ENTRY}")]);
        let on_disk = std::fs::read_to_string(dir.join(STATE_FILE)).unwrap();
        assert!(
            !on_disk.contains(&"07".repeat(AUTH_KEY_BYTES)),
            "auth key leaked to the state file"
        );

        let reloaded = KeychainSession::load(vault(store, &dir)).unwrap();
        assert!(reloaded.has_home_auth_key());
        assert_eq!(
            reloaded.dc_option(2).unwrap().unwrap().auth_key,
            Some([7u8; AUTH_KEY_BYTES])
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn update_state_is_buffered_until_flush() {
        let dir = temp_dir("flush");
        let store = Arc::new(MemoryStore::default());
        let session = KeychainSession::load(vault(store.clone(), &dir)).unwrap();
        session
            .set_update_state(UpdateState::Primary {
                pts: 5,
                date: 6,
                seq: 7,
            })
            .await
            .unwrap();
        assert!(
            !dir.join(STATE_FILE).exists(),
            "state must not be written before flush"
        );

        session.flush().unwrap();
        let reloaded = KeychainSession::load(vault(store, &dir)).unwrap();
        assert_eq!(reloaded.updates_state().await.unwrap().pts, 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wipe_removes_keys_and_state() {
        let dir = temp_dir("wipe");
        let store = Arc::new(MemoryStore::default());
        let session = KeychainSession::load(vault(store.clone(), &dir)).unwrap();
        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([1u8; AUTH_KEY_BYTES]);
        session.set_dc_option(&dc).await.unwrap();

        session.wipe().unwrap();
        assert!(store.entries().is_empty());
        assert!(!dir.join(STATE_FILE).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hex_round_trip_and_rejects_bad_keys() {
        let key = [0xabu8; AUTH_KEY_BYTES];
        assert_eq!(decode_key(&encode_hex(&key)).unwrap(), key);
        assert!(decode_key("abcd").is_err());
        assert!(decode_key(&"zz".repeat(AUTH_KEY_BYTES)).is_err());
    }
}
