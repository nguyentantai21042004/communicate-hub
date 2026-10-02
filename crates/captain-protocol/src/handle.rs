//! A cloneable handle for requests, usable while another task reads the update stream.

use std::sync::Arc;

use captain_core::{
    ChatId, ChatRef, DialogSummary, MessageId, MessageView, OutgoingMessage, SentMessage,
};
use grammers_client::peer::Peer;
use grammers_client::{Client, tl};

use crate::Result;
use crate::error::rpc;
use crate::inbound;
use crate::outbound::Outbound;
use crate::session::KeychainSession;

/// Sends and fetches on behalf of a logged-in [`crate::Connection`].
///
/// It shares the connection's network pool, so it stops working once the connection closes.
/// Unlike the connection it can be cloned into other tasks, because it never touches the
/// update stream.
#[derive(Clone)]
pub struct Handle {
    pub(crate) client: Client,
    pub(crate) session: Arc<KeychainSession>,
    pub(crate) self_id: i64,
}

impl Handle {
    /// Marked id of the logged-in account.
    pub fn self_id(&self) -> ChatId {
        ChatId(self.self_id)
    }

    fn outbound(&self) -> Outbound<'_> {
        Outbound {
            client: &self.client,
            session: &self.session,
            self_id: self.self_id,
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
        self.outbound().send(message).await
    }

    /// Fetches one message, or `None` if it does not exist or was deleted.
    ///
    /// # Errors
    ///
    /// [`crate::Error::UnknownChat`] when the chat is not in the peer cache, and
    /// [`crate::Error::Rpc`] when Telegram refuses the request.
    pub async fn message(&self, chat: ChatId, message: MessageId) -> Result<Option<MessageView>> {
        let peer = self.outbound().resolve(&ChatRef::Id(chat)).await?;
        let found = self
            .client
            .get_messages_by_id(peer, &[message.0])
            .await
            .map_err(rpc("messages.getMessages"))?;
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
        let peer = self.outbound().resolve(&ChatRef::Id(chat)).await?;
        // TELEGRAM: messages.getHistory leaves the read marker alone; only
        // messages.readHistory and channels.readHistory move it, and neither is called here.
        let mut iter = self.client.iter_messages(peer).limit(limit);
        if let Some(id) = before {
            iter = iter.offset_id(id.0);
        }
        let mut out = Vec::new();
        while let Some(m) = iter.next().await.map_err(rpc("messages.getHistory"))? {
            out.push(inbound::message_view(&m, self.self_id));
        }
        Ok(out)
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
        let peer = self.outbound().resolve(&ChatRef::Id(chat)).await?;
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

    /// Lists the most recent `limit` chats of the account, newest first. Reading the list does
    /// not mark anything as read.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Rpc`] when `messages.getDialogs` fails.
    pub async fn dialogs(&self, limit: usize) -> Result<Vec<DialogSummary>> {
        let mut iter = self.client.iter_dialogs().limit(limit);
        let mut out = Vec::new();
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
}

impl Handle {
    /// Walks the whole dialog list so grammers caches every chat's access hash. Returns how
    /// many chats it saw.
    pub(crate) async fn cache_peers(&self) -> Result<usize> {
        let mut iter = self.client.iter_dialogs();
        let mut seen = 0;
        while iter
            .next()
            .await
            .map_err(rpc("messages.getDialogs"))?
            .is_some()
        {
            seen += 1;
        }
        Ok(seen)
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
