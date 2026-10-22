//! A live MTProto connection for one profile: login, logout, and the core [`Protocol`].
//!
//! The connection owns two background tasks: the grammers I/O runner and a periodic session
//! flush. [`Connection::close`] stops both and writes the last session state; dropping the
//! connection without closing it aborts them and may lose up to one flush interval of peer
//! cache and update state (never the auth key, which is written immediately).

use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use captain_core::{
    ChatId, ChatKind, ChatSummary, InboundEvent, MessageId, OutgoingMessage, Protocol, SentMessage,
};
use captain_vault::{Secret, Vault};
use grammers_client::client::{SignInError, UpdateStream, UpdatesConfiguration};
use grammers_client::peer::User;
use grammers_client::sender::{ConnectionParams, SenderPool, SenderPoolHandle};
use grammers_client::session::updates::UpdatesLike;
use grammers_client::update::Update;
use grammers_client::{Client, InvocationError};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::task::JoinHandle;

use crate::error::{from_invocation, from_session_box, rpc};
use crate::handle::{Handle, HomeLink, NO_OPEN_SENDER};
use crate::inbound::{self, RawPayload};
use crate::session::KeychainSession;
use crate::{Error, Result};

// TELEGRAM: API terms 2.3 (https://core.telegram.org/api/terms) forbid a third-party app name
// containing "Telegram", so this is what the account's device list shows.
const DEVICE_MODEL: &str = "Captain (Rust)";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const PASSWORD_ATTEMPTS_MAX: usize = 3;
const UPDATE_QUEUE_LIMIT: usize = 1000; // user accounts in busy groups outgrow grammers' 100

/// Asks the person logging in for each login step. Implemented by the CLI.
pub trait LoginPrompt {
    /// Phone number in international format, for example `+84901234567`.
    ///
    /// # Errors
    ///
    /// Any I/O error reading the answer; login stops.
    fn phone(&mut self) -> std::io::Result<String>;

    /// Login code Telegram just sent to the account's other devices or by SMS.
    ///
    /// # Errors
    ///
    /// Any I/O error reading the answer; login stops.
    fn code(&mut self) -> std::io::Result<String>;

    /// Two-step verification (cloud) password. `hint` is the hint the user set, if any.
    /// Called again after a wrong password, up to a small fixed number of attempts.
    ///
    /// # Errors
    ///
    /// Any I/O error reading the answer; login stops.
    fn password(&mut self, hint: Option<&str>) -> std::io::Result<Secret>;
}

/// An open connection to Telegram for one vault profile.
pub struct Connection {
    clients: Arc<RwLock<Client>>,
    session: Arc<KeychainSession>,
    vault: Vault,
    api_hash: Secret,
    api_id: i32,
    pool: Arc<RwLock<SenderPoolHandle>>,
    runner: Option<JoinHandle<()>>,
    home: Arc<HomeLink>,
    home_watch: Option<JoinHandle<()>>,
    flusher: JoinHandle<()>,
    raw_updates: Option<UnboundedReceiver<grammers_client::session::updates::UpdatesLike>>,
    stream: Option<UpdateStream>,
    self_id: Option<i64>,
}

struct Network {
    client: Client,
    pool: SenderPoolHandle,
    runner: JoinHandle<()>,
    updates: UnboundedReceiver<grammers_client::session::updates::UpdatesLike>,
}

impl Connection {
    /// Opens a connection with the profile's API credentials and stored session. It does not
    /// check that the session is logged in; call [`Self::me`] or [`Self::login`] for that.
    ///
    /// # Errors
    ///
    /// [`Error::NoApiCredentials`] before `captain auth init`, and [`Error::Vault`] or
    /// [`Error::Session`] when stored state cannot be read.
    pub fn open(vault: Vault) -> Result<Self> {
        let api = vault
            .api_credentials()?
            .ok_or_else(|| Error::NoApiCredentials(vault.profile().to_string()))?;
        let session = Arc::new(
            KeychainSession::load(vault.clone()).map_err(|e| Error::Session(e.to_string()))?,
        );
        let home = Arc::new(HomeLink::new());
        let network = spawn_network(Arc::clone(&session), api.api_id);
        let pool = Arc::new(RwLock::new(network.pool.clone()));
        let (updates, home_watch) =
            watch_home_sender(Arc::clone(&home), home.generation(), network.updates);
        let flusher = tokio::spawn(flush_periodically(Arc::clone(&session)));
        Ok(Self {
            clients: Arc::new(RwLock::new(network.client)),
            session,
            vault,
            api_hash: api.api_hash,
            api_id: api.api_id,
            pool,
            runner: Some(network.runner),
            home,
            home_watch: Some(home_watch),
            flusher,
            raw_updates: Some(updates),
            stream: None,
            self_id: None,
        })
    }

    fn pool_handle(&self) -> SenderPoolHandle {
        match self.pool.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_pool(&self, handle: SenderPoolHandle) {
        match self.pool.write() {
            Ok(mut slot) => *slot = handle,
            Err(poisoned) => *poisoned.into_inner() = handle,
        }
    }

    /// Whether this process refused to replace a stored auth key.
    ///
    /// Once this is true, Telegram calls return [`Error::NotAuthorized`] and must not be retried.
    pub fn auth_key_conflict(&self) -> bool {
        self.session.auth_key_conflict()
    }

    /// `Some` when [`Self::auth_key_conflict`] is set. The error tells the owner to log in again.
    pub fn auth_key_block(&self) -> Option<Error> {
        self.auth_key_conflict().then(|| self.not_authorized())
    }

    fn not_authorized(&self) -> Error {
        Error::NotAuthorized(self.vault.profile().to_string())
    }

    fn client(&self) -> Client {
        match self.clients.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Stops the network tasks and starts them again on the same session.
    ///
    /// The previous pool is fully stopped before the new one connects, so Telegram does not
    /// see two connections for this auth key. [`Handle`] clones keep working: they read the
    /// client from a shared slot that this method replaces. Call [`Self::listen`] afterwards
    /// to receive updates. Saved update state is kept, so the new stream can catch up.
    /// A latched auth-key refusal returns immediately and does not start another pool.
    pub async fn restart_network(&mut self) {
        if self.session.auth_key_conflict() {
            return;
        }
        // Close the sender before the old pool stops, so a ping cannot open a new auth key
        // against the pool that is about to quit or the empty one that replaces it.
        self.home.retire();
        if let Some(watch) = self.home_watch.take() {
            watch.abort();
        }
        if let Some(stream) = self.stream.take()
            && let Err(e) = stream.sync_update_state().await
        {
            tracing::warn!(error = %e, "could not record the update position");
        }
        self.raw_updates.take();
        self.pool_handle().quit();
        if let Some(runner) = self.runner.take()
            && let Err(e) = runner.await
        {
            tracing::warn!(error = %e, "network task ended abnormally");
        }
        let network = spawn_network(Arc::clone(&self.session), self.api_id);
        let (updates, home_watch) = watch_home_sender(
            Arc::clone(&self.home),
            self.home.generation(),
            network.updates,
        );
        match self.clients.write() {
            Ok(mut slot) => *slot = network.client,
            Err(poisoned) => *poisoned.into_inner() = network.client,
        }
        self.set_pool(network.pool.clone());
        self.runner = Some(network.runner);
        self.home_watch = Some(home_watch);
        self.raw_updates = Some(updates);
        tracing::info!("telegram network restarted");
    }

    /// Returns the logged-in account.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`] when the session has no logged-in account.
    pub async fn me(&mut self) -> Result<ChatSummary> {
        // Without a key any request would first negotiate a fresh one and store it in the
        // Keychain, only to learn the session is logged out. A latched refusal means grammers
        // already minted one key this process will not store; do not mint another.
        if self.session.auth_key_conflict() || !self.session.has_home_auth_key() {
            return Err(self.not_authorized());
        }
        let generation = self.home.generation();
        match self.client().get_me().await {
            Ok(user) => {
                if self.session.auth_key_conflict() {
                    self.home.close_if(generation);
                    return Err(self.not_authorized());
                }
                if !self.home.set_open_if(generation) {
                    return Err(Error::Rpc {
                        request: "users.getUsers",
                        message: NO_OPEN_SENDER.to_owned(),
                    });
                }
                Ok(self.remember_me(&user))
            }
            Err(error) => {
                self.home.close_if(generation);
                if self.session.auth_key_conflict() {
                    return Err(self.not_authorized());
                }
                Err(from_invocation(
                    self.vault.profile().as_str(),
                    "users.getUsers",
                    &error,
                ))
            }
        }
    }

    /// Logs in interactively, or returns the account right away if already logged in.
    ///
    /// # Errors
    ///
    /// [`Error::Login`] for a wrong code, too many wrong passwords, a phone number without a
    /// Telegram account, or a prompt that failed to read input.
    pub async fn login(&mut self, prompt: &mut impl LoginPrompt) -> Result<ChatSummary> {
        // grammers writes the auth key through `set_dc_option` during sign-in. This is the only
        // caller allowed to replace a key that is already stored.
        let _replace = self.session.replace_auth_key();
        if self
            .client()
            .is_authorized()
            .await
            .map_err(rpc("updates.getState"))?
        {
            return self.me().await;
        }
        let phone = prompt.phone().map_err(login_io)?;
        let token = self
            .client()
            .request_login_code(phone.trim(), self.api_hash.expose())
            .await
            .map_err(rpc("auth.sendCode"))?;
        let code = prompt.code().map_err(login_io)?;

        let user = match self.client().sign_in(&token, code.trim()).await {
            Ok(user) => user,
            Err(SignInError::PasswordRequired(mut password_token)) => {
                let mut attempt = 1;
                loop {
                    let password = prompt.password(password_token.hint()).map_err(login_io)?;
                    match self
                        .client()
                        .check_password(password_token, password.expose())
                        .await
                    {
                        Ok(user) => break user,
                        Err(SignInError::InvalidPassword(retry))
                            if attempt < PASSWORD_ATTEMPTS_MAX =>
                        {
                            attempt += 1;
                            password_token = retry;
                        }
                        Err(e) => return Err(sign_in_error(e)),
                    }
                }
            }
            Err(e) => return Err(sign_in_error(e)),
        };
        self.session
            .flush()
            .map_err(|e| Error::Session(e.to_string()))?;
        Ok(self.remember_me(&user))
    }

    /// Ends the Telegram session on the server, then deletes the local auth key and state.
    /// The local data is deleted even if the server call fails, so a lost device can still be
    /// cleaned up offline.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when local data cannot be deleted. A failed server call is only
    /// logged: the key is gone locally either way, and Telegram expires unused sessions.
    pub async fn logout(self) -> Result<()> {
        if let Err(e) = self.client().sign_out().await {
            tracing::warn!(error = %e, "auth.logOut failed; deleting local session anyway");
        }
        self.session
            .wipe()
            .map_err(|e| Error::Session(e.to_string()))?;
        self.shutdown(false).await
    }

    /// Starts receiving updates. `catch_up` also delivers what arrived while offline, as far
    /// as the stored update state allows.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`] when not logged in.
    pub async fn listen(&mut self, catch_up: bool) -> Result<()> {
        if self.session.auth_key_conflict() {
            return Err(self.not_authorized());
        }
        self.me().await?;
        let Some(raw_updates) = self.raw_updates.take() else {
            return Ok(()); // already listening
        };
        let config = UpdatesConfiguration {
            catch_up,
            update_queue_limit: Some(UPDATE_QUEUE_LIMIT),
        };
        let stream = self
            .client()
            .stream_updates(raw_updates, config)
            .await
            .map_err(|e| from_session_box(self.vault.profile().as_str(), e))?;
        self.stream = Some(stream);
        Ok(())
    }

    /// Stops the background tasks after saving the update position and session state.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when the final state cannot be written.
    pub async fn close(self) -> Result<()> {
        self.shutdown(true).await
    }

    async fn shutdown(mut self, save: bool) -> Result<()> {
        let mut result = Ok(());
        if save {
            if let Some(stream) = &self.stream
                && let Err(e) = stream.sync_update_state().await
            {
                tracing::warn!(error = %e, "could not record the update position");
            }
            result = self
                .session
                .flush()
                .map_err(|e| Error::Session(e.to_string()));
        }
        self.home.retire();
        if let Some(watch) = self.home_watch.take() {
            watch.abort();
        }
        self.flusher.abort();
        self.pool_handle().quit();
        if let Some(runner) = self.runner.take()
            && let Err(e) = runner.await
        {
            tracing::warn!(error = %e, "network task ended abnormally");
        }
        result
    }

    fn remember_me(&mut self, user: &User) -> ChatSummary {
        let id = inbound::chat_id(user.id(), 0);
        self.self_id = Some(id.0);
        ChatSummary {
            id,
            kind: ChatKind::User,
            name: Some(user.full_name()),
            username: user.username().map(str::to_owned),
            is_bot: user.is_bot(),
        }
    }

    /// Caches the access hash of every chat in the dialog list, and returns how many chats it
    /// saw. Call it after [`Self::listen`] and before reading updates.
    ///
    /// TELEGRAM: filling a gap in a supergroup or channel needs its access hash, and grammers
    /// only caches hashes from requests such as `messages.getDialogs`, never from updates.
    /// Without this, catch-up silently skips any supergroup the session has not fetched.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`] before login or when the session is dead, and [`Error::Rpc`]
    /// when `messages.getDialogs` fails for another reason.
    pub async fn cache_peers(&self) -> Result<usize> {
        if self.session.auth_key_conflict() {
            return Err(self.not_authorized());
        }
        let seen = self.handle()?.cache_peers().await?;
        // Listing dialogs also stores each channel's current position in the session, which
        // would skip that channel's catch-up after a crash. Put the stream's position back.
        if let Some(stream) = &self.stream
            && let Err(e) = stream.sync_update_state().await
        {
            tracing::warn!(error = %e, "could not record the update position");
        }
        Ok(seen)
    }

    /// Returns a handle for sends and fetches that other tasks can hold while this connection
    /// reads updates.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`] before [`Self::me`] or [`Self::login`] has succeeded.
    pub fn handle(&self) -> Result<Handle> {
        let self_id = self
            .self_id
            .ok_or_else(|| Error::NotAuthorized(self.vault.profile().to_string()))?;
        Ok(Handle {
            clients: Arc::clone(&self.clients),
            session: Arc::clone(&self.session),
            self_id,
            home: Arc::clone(&self.home),
            pools: Arc::clone(&self.pool),
        })
    }
}

/// Forwards updates and closes [`HomeLink`] when the home sender task exits.
///
/// grammers emits [`UpdatesLike::ConnectionClosed`] and then drops that sender. The keepalive
/// must see the close before the next ping, or `invoke` would open a new connection.
fn watch_home_sender(
    link: Arc<HomeLink>,
    generation: u64,
    mut incoming: UnboundedReceiver<UpdatesLike>,
) -> (UnboundedReceiver<UpdatesLike>, JoinHandle<()>) {
    let (tx, rx) = unbounded_channel();
    let task = tokio::spawn(async move {
        while let Some(update) = incoming.recv().await {
            if matches!(update, UpdatesLike::ConnectionClosed) {
                link.close_if(generation);
            }
            if tx.send(update).is_err() {
                break;
            }
        }
        link.close_if(generation);
    });
    (rx, task)
}

fn connection_params() -> ConnectionParams {
    ConnectionParams {
        device_model: DEVICE_MODEL.to_owned(),
        app_version: APP_VERSION.to_owned(),
        ..ConnectionParams::default()
    }
}

fn spawn_network(session: Arc<KeychainSession>, api_id: i32) -> Network {
    let pool = SenderPool::with_configuration(session, api_id, connection_params());
    let quit = pool.handle.thin.clone();
    let runner = tokio::spawn(pool.runner.run());
    Network {
        client: Client::new(pool.handle),
        pool: quit,
        runner,
        updates: pool.updates,
    }
}

impl Protocol for Connection {
    type Error = Error;

    async fn next_event(&mut self) -> Result<Option<InboundEvent>> {
        let generation = self.home.generation();
        if self.session.auth_key_conflict() {
            self.home.close_if(generation);
            return Err(self.not_authorized());
        }
        if self.stream.is_none() {
            self.listen(false).await?;
        }
        let self_id = self.self_id.unwrap_or_default();
        let Some(stream) = self.stream.as_mut() else {
            return Ok(None);
        };
        let (raw, state, peers) = match stream.next_raw().await {
            Ok(next) => next,
            // The runner quit (shutdown or a dropped network); there are no more updates.
            Err(InvocationError::Dropped) => {
                self.home.close_if(generation);
                return Ok(None);
            }
            Err(error) => {
                if self.session.auth_key_conflict() {
                    self.home.close_if(generation);
                    return Err(self.not_authorized());
                }
                let mapped = from_invocation(
                    self.vault.profile().as_str(),
                    "updates.getDifference",
                    &error,
                );
                if matches!(mapped, Error::NotAuthorized(_)) {
                    self.home.close_if(generation);
                }
                return Err(mapped);
            }
        };
        // grammers hands the update position to the session only when asked. Without this a
        // crash would replay everything since the last clean close; with it, at most what
        // arrived since the last periodic flush.
        if let Err(e) = stream.sync_update_state().await {
            tracing::warn!(error = %e, "could not record the update position");
        }
        let payload = RawPayload::capture(&raw);
        let client = self.client();
        let update = Update::from_raw(&client, raw, state, peers);
        Ok(Some(inbound::to_event(
            update,
            payload,
            self_id,
            now_unix(),
        )))
    }

    async fn send(&self, message: OutgoingMessage) -> Result<Vec<SentMessage>> {
        self.handle()?.send(message).await
    }

    async fn download_media(&self, chat: ChatId, message: MessageId) -> Result<Option<Vec<u8>>> {
        self.handle()?.download_media(chat, message).await
    }
}

async fn flush_periodically(session: Arc<KeychainSession>) {
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    loop {
        tick.tick().await;
        if let Err(e) = session.flush() {
            tracing::warn!(error = %e, "session flush failed; will retry");
        }
    }
}

fn sign_in_error(e: SignInError) -> Error {
    Error::Login(match e {
        SignInError::SignUpRequired => {
            "this phone number has no Telegram account; sign up in an official app first".into()
        }
        SignInError::InvalidCode => "the login code is wrong or expired".into(),
        SignInError::InvalidPassword(_) => "wrong two-step verification password".into(),
        other => other.to_string(),
    })
}

#[expect(clippy::needless_pass_by_value, reason = "used as a map_err callback")]
fn login_io(e: std::io::Error) -> Error {
    Error::Login(format!("could not read input: {e}"))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use captain_core::Protocol;
    use captain_vault::{ApiCredentials, MemoryStore, Profile, Secret, Vault};
    use grammers_session::Session;

    use super::*;

    #[tokio::test]
    async fn me_listen_and_next_event_stop_after_an_auth_key_conflict() {
        let dir = std::env::temp_dir().join(format!("captain-conflict-{}", std::process::id()));
        let vault = Vault::with_state_dir(
            Arc::new(MemoryStore::default()),
            Profile::default(),
            dir.clone(),
        );
        vault
            .set_api_credentials(&ApiCredentials {
                api_id: 1,
                api_hash: Secret::new("test-hash"),
            })
            .unwrap();
        let prepared = KeychainSession::load(vault.clone()).unwrap();
        let mut dc = prepared.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([7u8; 256]);
        prepared.set_dc_option(&dc).await.unwrap();
        prepared.set_home_dc_id(2).await.unwrap();
        drop(prepared);

        let mut conn = Connection::open(vault).unwrap();
        let mut dc = conn.session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([9u8; 256]);
        let rejected = conn.session.set_dc_option(&dc).await.unwrap_err();
        assert!(rejected.is_auth_key_conflict(), "{rejected}");
        assert!(conn.auth_key_conflict());
        assert_eq!(conn.session.home_auth_key(), Some([7u8; 256]));

        let me = tokio::time::timeout(Duration::from_millis(200), conn.me()).await;
        let error = me
            .expect("me() called Telegram after a refusal")
            .unwrap_err();
        assert!(matches!(error, Error::NotAuthorized(_)), "{error}");
        assert!(error.to_string().contains("captain auth login"), "{error}");
        assert!(!error.to_string().contains("session error"), "{error}");

        let listen = tokio::time::timeout(Duration::from_millis(200), conn.listen(true)).await;
        let error = listen
            .expect("listen() called Telegram after a refusal")
            .unwrap_err();
        assert!(matches!(error, Error::NotAuthorized(_)), "{error}");

        let next =
            tokio::time::timeout(Duration::from_millis(200), Protocol::next_event(&mut conn)).await;
        let error = next
            .expect("next_event() called Telegram after a refusal")
            .unwrap_err();
        assert!(matches!(error, Error::NotAuthorized(_)), "{error}");

        let peers = tokio::time::timeout(Duration::from_millis(200), conn.cache_peers()).await;
        let error = peers
            .expect("cache_peers() called Telegram after a refusal")
            .unwrap_err();
        assert!(matches!(error, Error::NotAuthorized(_)), "{error}");
        assert_eq!(conn.session.refusals(), 1);
        conn.close().await.unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }
}
