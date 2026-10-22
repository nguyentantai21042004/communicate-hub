//! MTProto session storage split by sensitivity.
//!
//! SECURITY: the permanent auth keys are the account. Anyone holding one can act as the user
//! without a password or code. They live only in the Keychain. Everything else the session
//! tracks (datacenter addresses, the peer cache, the update state) is written to a `0600` JSON
//! file in the profile's state directory, so leaking that file does not leak the account.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
/// Refusals this process records before every later Telegram call is unauthorized.
///
/// grammers negotiates a fresh auth key before `set_dc_option`. One refusal means that
/// key was already minted and not stored. The next call must not mint another.
pub(crate) const AUTH_KEY_NEGOTIATION_CAP: u32 = 1;

/// grammers [`Session`] that keeps auth keys in the Keychain and the rest on disk.
///
/// Auth key and home datacenter changes are written through immediately; they are rare.
/// Peer cache and update state changes only mark the session dirty, and [`Self::flush`] writes
/// them. The connection flushes periodically and on shutdown.
///
/// A datacenter that already has an auth key keeps that key. [`Self::replace_auth_key`] is the
/// only way to overwrite it, and only [`crate::Connection::login`] holds that guard.
///
/// A refused replacement latches [`Self::auth_key_conflict`] for the rest of this process.
/// Callers stop instead of letting grammers negotiate another key.
pub struct KeychainSession {
    data: Mutex<SessionData>,
    vault: Vault,
    state_path: PathBuf,
    dirty: AtomicBool,
    allow_replace: Arc<AtomicBool>,
    /// Set when a stored key was kept. Independent of the refusal counter.
    auth_conflict: AtomicBool,
    /// How many replacements this process has refused.
    refusals: AtomicU32,
}

/// Held for the body of [`crate::Connection::login`] so sign-in may replace an auth key.
#[must_use = "dropping this ends the login replace window immediately"]
pub(crate) struct AuthKeyReplace {
    flag: Arc<AtomicBool>,
}

impl Drop for AuthKeyReplace {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Error raised by [`KeychainSession`] through the grammers [`Session`] trait.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The Keychain entry or the state file could not be read or written.
    #[error("session storage: {0}")]
    Storage(String),
    /// A new auth key was refused because this datacenter already has one.
    ///
    /// The stored key is unchanged. The owner has to run `captain auth login`.
    #[error("refusing to replace a stored auth key; run `captain auth login`")]
    AuthKeyConflict,
}

impl SessionError {
    /// Whether grammers tried to replace a stored auth key and this session kept the old one.
    pub(crate) fn is_auth_key_conflict(&self) -> bool {
        matches!(self, Self::AuthKeyConflict)
    }
}

/// True when either signal says this process must not negotiate another auth key.
///
/// `refusals` counts replacements that were turned down. At [`AUTH_KEY_NEGOTIATION_CAP`]
/// the process is unauthorized even when `flag` was not stored.
pub(crate) fn conflict_latched(flag: bool, refusals: u32) -> bool {
    flag || refusals >= AUTH_KEY_NEGOTIATION_CAP
}

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
            allow_replace: Arc::new(AtomicBool::new(false)),
            auth_conflict: AtomicBool::new(false),
            refusals: AtomicU32::new(0),
        })
    }

    /// Whether a stored auth key was kept and a replacement was refused.
    ///
    /// True when the flag is set or the refusal count has reached
    /// [`AUTH_KEY_NEGOTIATION_CAP`], so a missed flag store still stops the process.
    pub(crate) fn auth_key_conflict(&self) -> bool {
        conflict_latched(
            self.auth_conflict.load(Ordering::Acquire),
            self.refusals.load(Ordering::Acquire),
        )
    }

    /// How many auth-key replacements this process has refused.
    #[cfg(test)]
    pub(crate) fn refusals(&self) -> u32 {
        self.refusals.load(Ordering::Acquire)
    }

    /// Datacenter id the session uses for the account.
    pub(crate) fn home_dc(&self) -> i32 {
        self.lock().home_dc
    }

    fn note_auth_key_refusal(&self) {
        self.refusals.fetch_add(1, Ordering::AcqRel);
        self.auth_conflict.store(true, Ordering::Release);
    }

    fn clear_auth_key_conflict(&self) {
        self.auth_conflict.store(false, Ordering::Release);
        self.refusals.store(0, Ordering::Release);
    }

    /// Permits replacing a stored auth key until the guard is dropped.
    ///
    /// Only [`crate::Connection::login`] may call this. Every other `set_dc_option` that brings
    /// a different key for a datacenter that already has one is refused and leaves the stored
    /// key in place.
    pub(crate) fn replace_auth_key(&self) -> AuthKeyReplace {
        self.allow_replace.store(true, Ordering::Release);
        AuthKeyReplace {
            flag: Arc::clone(&self.allow_replace),
        }
    }

    pub(crate) fn home_auth_key(&self) -> Option<[u8; AUTH_KEY_BYTES]> {
        let data = self.lock();
        data.dc_options
            .get(&data.home_dc)
            .and_then(|dc| dc.auth_key)
    }

    pub(crate) fn profile_name(&self) -> &str {
        self.vault.profile().as_str()
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
            let dc_id = dc_option.id;
            // The lock must not cover `write_auth`: that call takes the lock again.
            let key_changed = {
                let mut data = self.lock();
                let old_key = data.dc_options.get(&dc_id).and_then(|dc| dc.auth_key);
                let replacing = old_key.is_some() && old_key != dc_option.auth_key;
                if replacing && !self.allow_replace.load(Ordering::Acquire) {
                    None
                } else {
                    let changed = old_key != dc_option.auth_key;
                    data.dc_options.insert(dc_id, dc_option);
                    Some(changed)
                }
            };
            let Some(key_changed) = key_changed else {
                self.note_auth_key_refusal();
                tracing::error!(
                    dc_id,
                    "refusing to replace a stored auth key outside captain auth login"
                );
                return Err(SessionError::AuthKeyConflict);
            };
            if key_changed {
                self.write_auth()?;
                // Login just stored the key it negotiated. Later calls in this process may use it.
                if self.allow_replace.load(Ordering::Acquire) {
                    self.clear_auth_key_conflict();
                }
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
    SessionError::Storage(e.to_string())
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
        SessionError::Storage(format!(
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

    #[tokio::test]
    async fn set_dc_option_rejects_a_different_auth_key_outside_login() {
        let dir = temp_dir("keep-key");
        let store = Arc::new(MemoryStore::default());
        let vault = vault(store.clone(), &dir);
        let session = KeychainSession::load(vault.clone()).unwrap();
        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([7u8; AUTH_KEY_BYTES]);
        session.set_dc_option(&dc).await.unwrap();
        session.set_home_dc_id(2).await.unwrap();
        let stored = vault.secret(AUTH_ENTRY).unwrap().unwrap();
        let original = stored.expose().to_owned();

        dc.auth_key = Some([9u8; AUTH_KEY_BYTES]);
        let rejected = session.set_dc_option(&dc).await.unwrap_err();
        assert!(rejected.is_auth_key_conflict(), "{rejected}");
        assert!(
            rejected.to_string().contains("captain auth login"),
            "{rejected}"
        );
        assert!(session.auth_key_conflict());
        assert_eq!(session.refusals(), 1);
        assert!(!rejected.to_string().contains("session error"));
        assert_eq!(
            session.dc_option(2).unwrap().unwrap().auth_key,
            Some([7u8; AUTH_KEY_BYTES])
        );
        assert_eq!(session.home_auth_key(), Some([7u8; AUTH_KEY_BYTES]));
        assert_eq!(
            vault.secret(AUTH_ENTRY).unwrap().unwrap().expose(),
            original
        );
        assert!(!original.contains(&encode_hex(&[9u8; AUTH_KEY_BYTES])));

        dc.auth_key = None;
        assert!(session.set_dc_option(&dc).await.is_err());
        assert_eq!(session.home_auth_key(), Some([7u8; AUTH_KEY_BYTES]));

        let mut same = session.dc_option(2).unwrap().unwrap();
        same.ipv4 = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(1, 2, 3, 4), 443);
        session.set_dc_option(&same).await.unwrap();
        assert_eq!(session.home_auth_key(), Some([7u8; AUTH_KEY_BYTES]));
        assert_eq!(
            vault.secret(AUTH_ENTRY).unwrap().unwrap().expose(),
            original
        );

        dc.auth_key = Some([9u8; AUTH_KEY_BYTES]);
        let _guard = session.replace_auth_key();
        session.set_dc_option(&dc).await.unwrap();
        assert_eq!(session.home_auth_key(), Some([9u8; AUTH_KEY_BYTES]));
        assert!(!session.auth_key_conflict());
        drop(_guard);
        dc.auth_key = Some([8u8; AUTH_KEY_BYTES]);
        assert!(
            session
                .set_dc_option(&dc)
                .await
                .unwrap_err()
                .is_auth_key_conflict()
        );
        assert!(session.auth_key_conflict());
        assert_eq!(session.home_auth_key(), Some([9u8; AUTH_KEY_BYTES]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    #[allow(
        clippy::panic,
        reason = "the replace guard has to drop while this login unwinds"
    )]
    async fn replace_guard_releases_on_error_and_on_panic() {
        let dir = temp_dir("guard");
        let session =
            Arc::new(KeychainSession::load(vault(Arc::new(MemoryStore::default()), &dir)).unwrap());
        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([7u8; AUTH_KEY_BYTES]);
        session.set_dc_option(&dc).await.unwrap();
        session.set_home_dc_id(2).await.unwrap();

        fn login_fails(session: &KeychainSession) -> Result<(), &'static str> {
            let _guard = session.replace_auth_key();
            Err("the login code was rejected")
        }
        assert!(login_fails(&session).is_err());
        dc.auth_key = Some([9u8; AUTH_KEY_BYTES]);
        assert!(
            session
                .set_dc_option(&dc)
                .await
                .unwrap_err()
                .is_auth_key_conflict()
        );
        assert_eq!(session.home_auth_key(), Some([7u8; AUTH_KEY_BYTES]));

        let cloned = Arc::clone(&session);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cloned.replace_auth_key();
            panic!("login failed");
        }));
        assert!(panicked.is_err());
        dc.auth_key = Some([8u8; AUTH_KEY_BYTES]);
        assert!(
            session
                .set_dc_option(&dc)
                .await
                .unwrap_err()
                .is_auth_key_conflict()
        );
        assert_eq!(session.home_auth_key(), Some([7u8; AUTH_KEY_BYTES]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn conflict_latches_on_the_flag_or_the_cap() {
        assert!(!conflict_latched(false, 0));
        assert!(conflict_latched(true, 0));
        assert!(conflict_latched(false, AUTH_KEY_NEGOTIATION_CAP));
    }

    #[test]
    fn hex_round_trip_and_rejects_bad_keys() {
        let key = [0xabu8; AUTH_KEY_BYTES];
        assert_eq!(decode_key(&encode_hex(&key)).unwrap(), key);
        assert!(decode_key("abcd").is_err());
        assert!(decode_key(&"zz".repeat(AUTH_KEY_BYTES)).is_err());
    }
}
