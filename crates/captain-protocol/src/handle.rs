//! A cloneable handle for requests, usable while another task reads the update stream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use captain_core::{
    ChatId, ChatRef, DialogSummary, MessageId, MessageView, OutgoingMessage, SentMessage,
};
use grammers_client::peer::Peer;
use grammers_client::sender::SenderPoolHandle;
use grammers_client::{Client, tl};

use crate::Result;
use crate::error::{from_invocation, rpc};
use crate::inbound;
use crate::outbound::Outbound;
use crate::session::KeychainSession;

/// Link failure returned when a keepalive has no home sender to use.
///
/// The text is matched by the daemon's link classifier (`ServerClosed` via this phrase) so a
/// closed sender reconnects through `restart_network` instead of negotiating an auth key.
pub(crate) const NO_OPEN_SENDER: &str = "no open home sender";

/// Whether the home MTProto sender is up. Ping reads it and does not connect when it is closed.
///
/// The generation and the open bit share one word so [`Self::close_if`] is a single
/// compare-and-swap. A retire that bumps the generation cannot lose to a stale close.
#[derive(Debug)]
pub(crate) struct HomeLink {
    state: AtomicU64, // generation << 1 | open
}

impl HomeLink {
    const OPEN: u64 = 1;

    pub(crate) fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    fn pack(generation: u64, open: bool) -> u64 {
        (generation << 1) | u64::from(open)
    }

    fn generation_of(state: u64) -> u64 {
        state >> 1
    }

    pub(crate) fn is_open(&self) -> bool {
        self.state.load(Ordering::Acquire) & Self::OPEN == Self::OPEN
    }

    /// Opens this generation. A retire that already moved on is left closed.
    pub(crate) fn set_open_if(&self, generation: u64) -> bool {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if Self::generation_of(current) != generation {
                return false;
            }
            if current & Self::OPEN == Self::OPEN {
                return true;
            }
            let next = current | Self::OPEN;
            if self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// The current sender answered a request. A later ping may use it and must not connect.
    #[cfg(test)]
    pub(crate) fn set_open(&self) {
        let _ = self.set_open_if(self.generation());
    }

    /// Drops the sender and ignores watchers started for the previous network.
    pub(crate) fn retire(&self) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            let next = Self::pack(Self::generation_of(current).wrapping_add(1), false);
            if self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        Self::generation_of(self.state.load(Ordering::Acquire))
    }

    /// Closes `generation` only. One compare-and-swap, so a newer generation stays as it is.
    pub(crate) fn close_if(&self, generation: u64) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            if Self::generation_of(current) != generation || current & Self::OPEN == 0 {
                return;
            }
            let next = current & !Self::OPEN;
            if self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

/// Sends and fetches on behalf of a logged-in [`crate::Connection`].
///
/// It shares the connection's client slot, so a reconnect swaps the pool underneath and
/// existing clones keep working. [`crate::Connection::close`] still stops them. Unlike the
/// connection it can be cloned into other tasks, because it never touches the update stream.
#[derive(Clone)]
pub struct Handle {
    pub(crate) clients: Arc<RwLock<Client>>,
    pub(crate) session: Arc<KeychainSession>,
    pub(crate) self_id: i64,
    pub(crate) home: Arc<HomeLink>,
    pub(crate) pools: Arc<RwLock<SenderPoolHandle>>,
}

impl Handle {
    /// Marked id of the logged-in account.
    pub fn self_id(&self) -> ChatId {
        ChatId(self.self_id)
    }

    fn client(&self) -> Client {
        match self.clients.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn pool(&self) -> SenderPoolHandle {
        match self.pools.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Drops the home sender a call may have pushed after the link had already closed.
    fn drop_home_sender(&self) {
        let _ = self.pool().disconnect_from_dc(self.session.home_dc());
    }

    /// Reports a refused key or a link that closed while `invoke` was in `create_connection`.
    ///
    /// `discard_on_close` drops a successful result. Ping uses it. A send keeps its result
    /// after the stray sender is removed, so a delivered message is not reported as lost.
    fn finish_call<T>(
        &self,
        request: &'static str,
        generation: u64,
        before: Option<[u8; 256]>,
        result: Result<T>,
        discard_on_close: bool,
    ) -> Result<T> {
        let key_changed = before.is_some() && self.session.home_auth_key() != before;
        if self.session.auth_key_conflict() || key_changed {
            self.home.close_if(generation);
            self.drop_home_sender();
            return Err(crate::Error::NotAuthorized(
                self.session.profile_name().to_owned(),
            ));
        }
        if !self.home.is_open() || self.home.generation() != generation {
            self.drop_home_sender();
            if discard_on_close {
                return Err(crate::Error::Rpc {
                    request,
                    message: NO_OPEN_SENDER.to_owned(),
                });
            }
        }
        result
    }

    /// `Err` when this call must not open a connection or negotiate an auth key.
    ///
    /// A latched auth-key conflict wins over a closed link, so the supervisor stops
    /// instead of reconnecting into another `connect()`.
    fn require_sender(&self, request: &'static str) -> Result<()> {
        if self.session.auth_key_conflict() {
            return Err(crate::Error::NotAuthorized(
                self.session.profile_name().to_owned(),
            ));
        }
        if self.home.is_open() {
            Ok(())
        } else {
            Err(crate::Error::Rpc {
                request,
                message: NO_OPEN_SENDER.to_owned(),
            })
        }
    }

    fn outbound<'a>(&'a self, client: &'a Client) -> Outbound<'a> {
        Outbound {
            client,
            session: &self.session,
            self_id: self.self_id,
        }
    }

    /// Sends a keepalive ping.
    ///
    /// A response means this handle can still reach Telegram. It does not produce an update.
    ///
    /// # Errors
    ///
    /// [`crate::Error::NotAuthorized`] when the session or auth key is dead, and
    /// [`crate::Error::Rpc`] when the ping cannot be sent for another reason. A closed home
    /// sender fails before any request, so the ping cannot negotiate a new auth key. A link
    /// that closes during the ping drops that result and the sender the ping created.
    pub async fn ping(&self) -> Result<()> {
        self.require_sender("ping")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let invoked = self
            .client()
            .invoke(&tl::functions::Ping { ping_id: 0 })
            .await
            .map(|_| ())
            .map_err(|error| from_invocation(self.session.profile_name(), "ping", &error));
        // The open check above can pass, then the watcher closes the link while `invoke`
        // is in `create_connection`. The result belongs to a sender this ping must not keep.
        match self.finish_call("ping", generation, before, invoked, true) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.home.close_if(generation);
                self.drop_home_sender();
                Err(error)
            }
        }
    }

    /// Validates and sends one message or album. An album returns one entry per item.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Invalid`] before any network call when the message breaks a limit,
    /// [`crate::Error::UnknownChat`] for an unresolvable destination, and
    /// [`crate::Error::Rpc`] when Telegram refuses the request.
    pub async fn send(&self, message: OutgoingMessage) -> Result<Vec<SentMessage>> {
        self.require_sender("messages.sendMessage")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let client = self.client();
        let sent = self.outbound(&client).send(message).await;
        self.finish_call("messages.sendMessage", generation, before, sent, false)
    }

    /// Fetches one message, or `None` if it does not exist or was deleted.
    ///
    /// # Errors
    ///
    /// [`crate::Error::UnknownChat`] when the chat is not in the peer cache, and
    /// [`crate::Error::Rpc`] when Telegram refuses the request.
    pub async fn message(&self, chat: ChatId, message: MessageId) -> Result<Option<MessageView>> {
        self.require_sender("messages.getMessages")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let client = self.client();
        let found = async {
            let peer = self.outbound(&client).resolve(&ChatRef::Id(chat)).await?;
            client
                .get_messages_by_id(peer, &[message.0])
                .await
                .map_err(rpc("messages.getMessages"))
        }
        .await;
        let found = self.finish_call("messages.getMessages", generation, before, found, false)?;
        Ok(found
            .into_iter()
            .flatten()
            .next()
            .map(|m| inbound::message_view(&m, self.self_id)))
    }

    /// Fetches up to `limit` messages older than `before`, or the newest ones when it is
    /// `None`, newest first. Reading history does not mark anything as read.
    ///
    /// # Errors
    ///
    /// [`crate::Error::UnknownChat`] when the chat is not in the peer cache, and
    /// [`crate::Error::Rpc`] when Telegram refuses `messages.getHistory`.
    pub async fn history(
        &self,
        chat: ChatId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<MessageView>> {
        self.require_sender("messages.getHistory")?;
        let generation = self.home.generation();
        let key_before = self.session.home_auth_key();
        let client = self.client();
        let listed = async {
            let peer = self.outbound(&client).resolve(&ChatRef::Id(chat)).await?;
            // TELEGRAM: messages.getHistory leaves the read marker alone; only
            // messages.readHistory and channels.readHistory move it, and neither is called here.
            let mut iter = client.iter_messages(peer).limit(limit);
            if let Some(id) = before {
                iter = iter.offset_id(id.0);
            }
            let mut out = Vec::new();
            while let Some(m) = iter.next().await.map_err(rpc("messages.getHistory"))? {
                out.push(inbound::message_view(&m, self.self_id));
            }
            Ok(out)
        }
        .await;
        self.finish_call("messages.getHistory", generation, key_before, listed, false)
    }

    /// Downloads the media attached to a message, or `None` if it has none.
    ///
    /// # Errors
    ///
    /// Same as [`Self::message`], plus [`crate::Error::Rpc`] from `upload.getFile`.
    pub async fn download_media(
        &self,
        chat: ChatId,
        message: MessageId,
    ) -> Result<Option<Vec<u8>>> {
        self.require_sender("messages.getMessages")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let client = self.client();
        let downloaded = async {
            let peer = self.outbound(&client).resolve(&ChatRef::Id(chat)).await?;
            let found = client
                .get_messages_by_id(peer, &[message.0])
                .await
                .map_err(rpc("messages.getMessages"))?;
            let Some(media) = found.into_iter().flatten().next().and_then(|m| m.media()) else {
                return Ok(None);
            };
            let mut download = client.iter_download(&media);
            let mut bytes = Vec::new();
            while let Some(chunk) = download.next().await.map_err(rpc("upload.getFile"))? {
                bytes.extend_from_slice(&chunk);
            }
            Ok(Some(bytes))
        }
        .await;
        self.finish_call(
            "messages.getMessages",
            generation,
            before,
            downloaded,
            false,
        )
    }

    /// Lists the most recent `limit` chats of the account, newest first. Reading the list does
    /// not mark anything as read.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Rpc`] when `messages.getDialogs` fails.
    pub async fn dialogs(&self, limit: usize) -> Result<Vec<DialogSummary>> {
        self.require_sender("messages.getDialogs")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let mut iter = self.client().iter_dialogs().limit(limit);
        let mut out = Vec::new();
        let listed = async {
            while let Some(dialog) = iter.next().await.map_err(rpc("messages.getDialogs"))? {
                let unread_count = match &dialog.raw {
                    tl::enums::Dialog::Dialog(d) => d.unread_count,
                    tl::enums::Dialog::Folder(_) => continue,
                };
                out.push(DialogSummary {
                    chat: inbound::summary(dialog.peer_id(), Some(&dialog.peer), self.self_id),
                    forum: is_forum(&dialog.peer),
                    unread_count,
                    last_message_unix: dialog.last_message.as_ref().map(|m| m.date().timestamp()),
                });
            }
            Ok(out)
        }
        .await;
        self.finish_call("messages.getDialogs", generation, before, listed, false)
    }
}

impl Handle {
    /// Walks the whole dialog list so grammers caches every chat's access hash. Returns how
    /// many chats it saw.
    pub(crate) async fn cache_peers(&self) -> Result<usize> {
        self.require_sender("messages.getDialogs")?;
        let generation = self.home.generation();
        let before = self.session.home_auth_key();
        let mut iter = self.client().iter_dialogs();
        let mut seen = 0;
        let counted = async {
            while iter
                .next()
                .await
                .map_err(|error| {
                    from_invocation(self.session.profile_name(), "messages.getDialogs", &error)
                })?
                .is_some()
            {
                seen += 1;
            }
            Ok(seen)
        }
        .await;
        self.finish_call("messages.getDialogs", generation, before, counted, false)
    }
}

fn is_forum(peer: &Peer) -> bool {
    match peer {
        Peer::Channel(c) => c.raw.forum,
        Peer::Group(g) => matches!(&g.raw, tl::enums::Chat::Channel(c) if c.forum),
        Peer::User(_) => false,
    }
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle")
            .field("self_id", &self.self_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use captain_vault::{MemoryStore, Profile, Vault};
    use grammers_client::Client;
    use grammers_client::sender::{ConnectionParams, SenderPool};
    use grammers_session::Session;

    use super::*;

    #[tokio::test]
    async fn ping_when_the_home_sender_is_gone_does_not_create_a_key() {
        let dir = std::env::temp_dir().join(format!("captain-ping-closed-{}", std::process::id()));
        let store = Arc::new(MemoryStore::default());
        let session = Arc::new(
            KeychainSession::load(Vault::with_state_dir(
                store,
                Profile::default(),
                dir.clone(),
            ))
            .unwrap(),
        );
        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([4u8; 256]);
        session.set_dc_option(&dc).await.unwrap();
        session.set_home_dc_id(2).await.unwrap();

        let pool =
            SenderPool::with_configuration(Arc::clone(&session), 1, ConnectionParams::default());
        // Hold the runner so a mistaken invoke waits forever instead of failing closed.
        let _runner = pool.runner;
        let thin = pool.handle.thin.clone();
        let home = Arc::new(HomeLink::new());
        assert!(!home.is_open());
        let handle = Handle {
            clients: Arc::new(RwLock::new(Client::new(pool.handle))),
            session: Arc::clone(&session),
            self_id: 1,
            home,
            pools: Arc::new(RwLock::new(thin)),
        };
        let result = tokio::time::timeout(Duration::from_millis(200), handle.ping()).await;
        let error = result
            .expect("ping waited on a new connection")
            .unwrap_err();
        assert!(error.to_string().contains(NO_OPEN_SENDER), "{error}");
        assert_eq!(session.home_auth_key(), Some([4u8; 256]));
        let _ = std::fs::remove_dir_all(dir);
    }

    struct Rig {
        session: Arc<KeychainSession>,
        home: Arc<HomeLink>,
        handle: Handle,
        runner: grammers_client::sender::SenderPoolRunner,
        dir: std::path::PathBuf,
    }

    async fn rig(tag: &str, open: bool) -> Rig {
        let dir = std::env::temp_dir().join(format!("captain-ping-{tag}-{}", std::process::id()));
        let session = Arc::new(
            KeychainSession::load(Vault::with_state_dir(
                Arc::new(MemoryStore::default()),
                Profile::default(),
                dir.clone(),
            ))
            .unwrap(),
        );
        let mut dc = session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([4u8; 256]);
        session.set_dc_option(&dc).await.unwrap();
        session.set_home_dc_id(2).await.unwrap();
        let pool =
            SenderPool::with_configuration(Arc::clone(&session), 1, ConnectionParams::default());
        let thin = pool.handle.thin.clone();
        let home = Arc::new(HomeLink::new());
        if open {
            home.set_open();
        }
        let handle = Handle {
            clients: Arc::new(RwLock::new(Client::new(pool.handle))),
            session: Arc::clone(&session),
            self_id: 1,
            home: Arc::clone(&home),
            pools: Arc::new(RwLock::new(thin)),
        };
        Rig {
            session,
            home,
            handle,
            runner: pool.runner,
            dir,
        }
    }

    #[tokio::test]
    async fn ping_after_an_auth_key_conflict_does_not_invoke() {
        let rig = rig("conflict", true).await;
        let mut dc = rig.session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([9u8; 256]);
        assert!(
            rig.session
                .set_dc_option(&dc)
                .await
                .unwrap_err()
                .is_auth_key_conflict()
        );
        assert!(rig.home.is_open());
        let _runner = rig.runner;
        let result = tokio::time::timeout(Duration::from_millis(200), rig.handle.ping()).await;
        let error = result
            .expect("ping negotiated another auth key after a refusal")
            .unwrap_err();
        assert!(matches!(error, crate::Error::NotAuthorized(_)), "{error}");
        assert!(error.to_string().contains("captain auth login"), "{error}");
        assert_eq!(rig.session.home_auth_key(), Some([4u8; 256]));
        assert_eq!(rig.session.refusals(), 1);

        let again = tokio::time::timeout(Duration::from_millis(200), rig.handle.ping()).await;
        again
            .expect("second ping negotiated another auth key")
            .unwrap_err();
        assert_eq!(rig.session.refusals(), 1);
        let _ = std::fs::remove_dir_all(rig.dir);
    }

    #[tokio::test]
    async fn closed_link_with_a_conflict_is_unauthorized_not_a_reconnect() {
        let rig = rig("conflict-closed", false).await;
        let mut dc = rig.session.dc_option(2).unwrap().unwrap();
        dc.auth_key = Some([9u8; 256]);
        rig.session.set_dc_option(&dc).await.unwrap_err();
        let _runner = rig.runner;
        let error = tokio::time::timeout(Duration::from_millis(200), rig.handle.ping())
            .await
            .expect("ping invoked")
            .unwrap_err();
        assert!(matches!(error, crate::Error::NotAuthorized(_)), "{error}");
        assert!(!error.to_string().contains(NO_OPEN_SENDER), "{error}");
        let _ = std::fs::remove_dir_all(rig.dir);
    }

    #[tokio::test]
    async fn ping_discards_the_result_when_the_link_closes_in_flight() {
        let rig = rig("race", true).await;
        let generation = rig.home.generation();
        let home = Arc::clone(&rig.home);
        let session = Arc::clone(&rig.session);
        let handle = rig.handle;
        let runner = rig.runner;
        let ping = tokio::spawn(async move { handle.ping().await });
        let mut finished_early = false;
        for _ in 0..50 {
            tokio::task::yield_now().await;
            if ping.is_finished() {
                finished_early = true;
                break;
            }
        }
        assert!(!finished_early, "ping finished before the link closed");
        home.close_if(generation);
        drop(runner);
        let error = ping.await.unwrap().unwrap_err();
        assert!(error.to_string().contains(NO_OPEN_SENDER), "{error}");
        assert_eq!(session.home_auth_key(), Some([4u8; 256]));
        assert!(!session.auth_key_conflict());
        let _ = std::fs::remove_dir_all(rig.dir);
    }

    #[tokio::test]
    async fn cache_peers_when_the_home_sender_is_gone_does_not_invoke() {
        let rig = rig("peers", false).await;
        let _runner = rig.runner;
        let error = tokio::time::timeout(Duration::from_millis(200), rig.handle.cache_peers())
            .await
            .expect("cache_peers opened a connection")
            .unwrap_err();
        assert!(error.to_string().contains(NO_OPEN_SENDER), "{error}");
        assert_eq!(rig.session.home_auth_key(), Some([4u8; 256]));
        let _ = std::fs::remove_dir_all(rig.dir);
    }

    #[test]
    fn close_if_does_not_close_a_newer_generation() {
        let link = HomeLink::new();
        link.set_open();
        let old = link.generation();
        link.retire();
        link.set_open();
        assert!(link.is_open());
        link.close_if(old);
        assert!(link.is_open(), "stale close cleared the new generation");
        assert_eq!(link.generation(), old + 1);
        link.close_if(link.generation());
        assert!(!link.is_open());
    }

    #[test]
    fn close_if_loses_a_race_with_retire() {
        let link = Arc::new(HomeLink::new());
        link.set_open();
        let old = link.generation();
        let closer = Arc::clone(&link);
        let worker = std::thread::spawn(move || {
            for _ in 0..20_000 {
                closer.close_if(old);
            }
        });
        link.retire();
        link.set_open();
        worker.join().unwrap();
        assert!(link.is_open());
        assert_eq!(link.generation(), old + 1);
    }
}
