//! A live MTProto connection for one profile: login, logout, and the core [`Protocol`].
//!
//! The connection owns two background tasks: the grammers I/O runner and a periodic session
//! flush. [`Connection::close`] stops both and writes the last session state; dropping the
//! connection without closing it aborts them and may lose up to one flush interval of peer
//! cache and update state (never the auth key, which is written immediately).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use captain_core::{
    ChatId, ChatKind, ChatRef, ChatSummary, InboundEvent, MessageId, OutgoingMessage, Protocol,
    SentMessage,
};
use captain_vault::{Secret, Vault};
use grammers_client::client::{SignInError, UpdateStream, UpdatesConfiguration};
use grammers_client::peer::User;
use grammers_client::sender::{ConnectionParams, SenderPool, SenderPoolHandle};
use grammers_client::update::Update;
use grammers_client::{Client, InvocationError};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use crate::error::rpc;
use crate::inbound::{self, RawPayload};
use crate::outbound::Outbound;
use crate::session::KeychainSession;
use crate::{Error, Result};

// TELEGRAM: API terms 2.3 (https://core.telegram.org/api/terms) forbid a third-party app name
// containing "Telegram", so this is what the account's device list shows.
const DEVICE_MODEL: &str = "Captain (Rust)";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const PASSWORD_ATTEMPTS_MAX: usize = 3;
const UPDATE_QUEUE_LIMIT: usize = 1000; // user accounts in busy groups outgrow grammers' 100
const UNAUTHORIZED: i32 = 401; // RPC error code for a session without a logged-in user

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
    client: Client,
    session: Arc<KeychainSession>,
    vault: Vault,
    api_hash: Secret,
    pool: SenderPoolHandle,
    runner: JoinHandle<()>,
    flusher: JoinHandle<()>,
    raw_updates: Option<UnboundedReceiver<grammers_client::session::updates::UpdatesLike>>,
    stream: Option<UpdateStream>,
    self_id: Option<i64>,
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
        let params = ConnectionParams {
            device_model: DEVICE_MODEL.to_owned(),
            app_version: APP_VERSION.to_owned(),
            ..ConnectionParams::default()
        };
        let pool = SenderPool::with_configuration(Arc::clone(&session), api.api_id, params);
        let quit_handle = pool.handle.thin.clone();
        let runner = tokio::spawn(pool.runner.run());
        let flusher = tokio::spawn(flush_periodically(Arc::clone(&session)));
        Ok(Self {
            client: Client::new(pool.handle),
            session,
            vault,
            api_hash: api.api_hash,
            pool: quit_handle,
            runner,
            flusher,
            raw_updates: Some(pool.updates),
            stream: None,
            self_id: None,
        })
    }

    /// Returns the logged-in account.
    ///
    /// # Errors
    ///
    /// [`Error::NotAuthorized`] when the session has no logged-in account.
    pub async fn me(&mut self) -> Result<ChatSummary> {
        // Without a key any request would first negotiate a fresh one and store it in the
        // Keychain, only to learn the session is logged out.
        if !self.session.has_home_auth_key() {
            return Err(Error::NotAuthorized(self.vault.profile().to_string()));
        }
        match self.client.get_me().await {
            Ok(user) => Ok(self.remember_me(&user)),
            Err(InvocationError::Rpc(e)) if e.code == UNAUTHORIZED => {
                Err(Error::NotAuthorized(self.vault.profile().to_string()))
            }
            Err(e) => Err(rpc("users.getUsers")(e)),
        }
    }

    /// Logs in interactively, or returns the account right away if already logged in.
    ///
    /// # Errors
    ///
    /// [`Error::Login`] for a wrong code, too many wrong passwords, a phone number without a
    /// Telegram account, or a prompt that failed to read input.
    pub async fn login(&mut self, prompt: &mut impl LoginPrompt) -> Result<ChatSummary> {
        if self
            .client
            .is_authorized()
            .await
            .map_err(rpc("updates.getState"))?
        {
            return self.me().await;
        }
        let phone = prompt.phone().map_err(login_io)?;
        let token = self
            .client
            .request_login_code(phone.trim(), self.api_hash.expose())
            .await
            .map_err(rpc("auth.sendCode"))?;
        let code = prompt.code().map_err(login_io)?;

        let user = match self.client.sign_in(&token, code.trim()).await {
            Ok(user) => user,
            Err(SignInError::PasswordRequired(mut password_token)) => {
                let mut attempt = 1;
                loop {
                    let password = prompt.password(password_token.hint()).map_err(login_io)?;
                    match self
                        .client
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
        if let Err(e) = self.client.sign_out().await {
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
        self.me().await?;
        let Some(raw_updates) = self.raw_updates.take() else {
            return Ok(()); // already listening
        };
        let config = UpdatesConfiguration {
            catch_up,
            update_queue_limit: Some(UPDATE_QUEUE_LIMIT),
        };
        let stream = self
            .client
            .stream_updates(raw_updates, config)
            .await
            .map_err(|e| Error::Session(e.to_string()))?;
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

    async fn shutdown(self, save: bool) -> Result<()> {
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
        self.flusher.abort();
        self.pool.quit();
        if let Err(e) = self.runner.await {
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
        }
    }

    async fn outbound(&self) -> Result<Outbound<'_>> {
        let self_id = self
            .self_id
            .ok_or_else(|| Error::NotAuthorized(self.vault.profile().to_string()))?;
        Ok(Outbound {
            client: &self.client,
            session: &self.session,
            self_id,
        })
    }
}

impl Protocol for Connection {
    type Error = Error;

    async fn next_event(&mut self) -> Result<Option<InboundEvent>> {
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
            Err(InvocationError::Dropped) => return Ok(None),
            Err(e) => return Err(rpc("updates.getDifference")(e)),
        };
        let payload = RawPayload::capture(&raw);
        let update = Update::from_raw(&self.client, raw, state, peers);
        Ok(Some(inbound::to_event(
            update,
            payload,
            self_id,
            now_unix(),
        )))
    }

    async fn send(&self, message: OutgoingMessage) -> Result<Vec<SentMessage>> {
        self.outbound().await?.send(message).await
    }

    async fn download_media(&self, chat: ChatId, message: MessageId) -> Result<Option<Vec<u8>>> {
        let outbound = self.outbound().await?;
        let peer = outbound.resolve(&ChatRef::Id(chat)).await?;
        let found = self
            .client
            .get_messages_by_id(peer, &[message.0])
            .await
            .map_err(rpc("messages.getMessages"))?;
        let Some(media) = found.into_iter().flatten().next().and_then(|m| m.media()) else {
            return Ok(None);
        };
        let mut download = self.client.iter_download(&media);
        let mut bytes = Vec::new();
        while let Some(chunk) = download.next().await.map_err(rpc("upload.getFile"))? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
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
