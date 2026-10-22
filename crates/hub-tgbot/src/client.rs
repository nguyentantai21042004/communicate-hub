//! Bot API client.
//!
//! [`BotClient`] implements [`TopicSink`]. Methods that the trait does not cover
//! (topic icon, reopen, edit, photo albums) live on the client and return
//! [`crate::Error`], which keeps [`Error::TooLarge`] and [`Error::LacksRights`] distinct.
//! The trait methods map those into [`hub_core::Error::Adapter`].

use std::future::Future;
use std::sync::Arc;

use frankenstein::AsyncTelegramApi;
use frankenstein::ParseMode;
use frankenstein::client_reqwest::Bot as ApiBot;
use frankenstein::input_file::FileUpload;
use frankenstein::input_media::{InputMediaPhoto, MediaGroupInputMedia};
use frankenstein::methods::{
    CloseForumTopicParams, CreateForumTopicParams, DeleteMessageParams, EditForumTopicParams,
    EditMessageTextParams, GetChatMemberParams, GetChatParams, GetUpdatesParams,
    PinChatMessageParams, ReopenForumTopicParams, SendAnimationParams, SendAudioParams,
    SendDocumentParams, SendMediaGroupParams, SendMessageParams, SendPhotoParams, SendVideoParams,
    SendVoiceParams, SetMessageReactionParams,
};
use frankenstein::response::{MessageOrBool, MethodResponse};
use frankenstein::stickers::Sticker;
use frankenstein::types::{
    AllowedUpdate, ChatFullInfo, ChatMember, Message, ReactionType, ReactionTypeEmoji,
    ReplyParameters, User,
};
use frankenstein::updates::Update;
use hub_core::{Attachment, TgMessageId, TopicId, TopicSink};
use tokio::sync::{mpsc, watch};

use crate::config::BotConfig;
use crate::error::{Error, Result};
use crate::incoming::{self, OffsetStore, TopicIncoming};
use crate::limits::{self, ALBUM_MAX_ITEMS, LONG_POLL_TIMEOUT_SECS};
use crate::media::{self, MediaMethod, PreparedFile};
use crate::preflight::{self, PreflightReport};
use crate::queue::RequestQueue;

/// Icon color `createForumTopic` will accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicIconColor {
    /// Blue, `0x6FB9F0`.
    Blue,
    /// Yellow, `0xFFD67E`.
    Yellow,
    /// Purple, `0xCB86DB`.
    Purple,
    /// Green, `0x8EEE98`.
    Green,
    /// Pink, `0xFF93B2`.
    Pink,
    /// Red, `0xFB6F5F`.
    Red,
}

impl TopicIconColor {
    /// The integer `createForumTopic` expects for this color.
    pub const fn rgb(self) -> u32 {
        match self {
            Self::Blue => limits::ICON_COLOR_BLUE,
            Self::Yellow => limits::ICON_COLOR_YELLOW,
            Self::Purple => limits::ICON_COLOR_PURPLE,
            Self::Green => limits::ICON_COLOR_GREEN,
            Self::Pink => limits::ICON_COLOR_PINK,
            Self::Red => limits::ICON_COLOR_RED,
        }
    }
}

/// Optional icon for [`BotClient::create_topic_with`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopicOptions {
    /// One of the six colors `Telegram` accepts. Ignored when `None`.
    pub icon_color: Option<TopicIconColor>,
    /// Custom emoji id for the topic icon. Ignored when `None`.
    pub icon_custom_emoji_id: Option<String>,
}

/// Client for one bridge supergroup.
///
/// Clone shares the rate-limit queue. Dropping a clone does not cancel calls
/// that are already in flight on another clone.
#[derive(Clone)]
pub struct BotClient {
    api: ApiBot,
    /// Own connection for `getUpdates`, so the long poll does not share the send pool.
    poll_api: ApiBot,
    group_id: i64,
    token: crate::config::BotToken,
    queue: RequestQueue,
    icons: Arc<tokio::sync::Mutex<Option<IconSet>>>,
}

#[derive(Clone)]
struct IconSet {
    person: TopicOptions,
    group: TopicOptions,
}

impl IconSet {
    fn colors_only() -> Self {
        Self {
            person: TopicOptions {
                icon_color: Some(TopicIconColor::Blue),
                icon_custom_emoji_id: None,
            },
            group: TopicOptions {
                icon_color: Some(TopicIconColor::Green),
                icon_custom_emoji_id: None,
            },
        }
    }

    fn from_stickers(stickers: &[Sticker]) -> Self {
        Self {
            person: pick_icon(stickers, limits::PERSON_EMOJI, TopicIconColor::Blue),
            group: pick_icon(stickers, limits::GROUP_EMOJI, TopicIconColor::Green),
        }
    }

    fn for_title(&self, title: &str) -> TopicOptions {
        if title.split_whitespace().nth(1) == Some(limits::GROUP_MARK) {
            self.group.clone()
        } else {
            self.person.clone()
        }
    }
}

fn pick_icon(stickers: &[Sticker], prefer: &[&str], color: TopicIconColor) -> TopicOptions {
    for emoji in prefer {
        if let Some(id) = stickers
            .iter()
            .find(|sticker| sticker.emoji.as_deref() == Some(*emoji))
            .and_then(|sticker| sticker.custom_emoji_id.clone())
        {
            return TopicOptions {
                icon_color: None,
                icon_custom_emoji_id: Some(id),
            };
        }
    }
    TopicOptions {
        icon_color: Some(color),
        icon_custom_emoji_id: None,
    }
}

impl std::fmt::Debug for BotClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotClient")
            .field("group_id", &self.group_id)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl BotClient {
    /// Builds a client that posts to `config.api_base_url`.
    ///
    /// The token is appended as `/bot<token>` and is not part of [`Debug`](std::fmt::Debug).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Permanent`] when `api_base_url` is empty or already contains the token.
    pub fn new(config: BotConfig) -> Result<Self> {
        let base = config.api_base_url.trim().trim_end_matches('/');
        // SECURITY: BotConfig debug-prints the base URL. Reject a token stashed in it.
        if base.is_empty() || base.contains(config.token.expose()) {
            return Err(Error::Permanent { status: None });
        }
        let api_url = format!("{base}/bot{}", config.token.expose());
        Ok(Self {
            api: ApiBot::new_url(api_url.clone()),
            poll_api: ApiBot::new_url(api_url),
            group_id: config.group_id,
            token: config.token,
            queue: RequestQueue::new(),
            icons: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// Opens a forum topic with no icon color and no custom emoji.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RateLimited`] after the retry budget is spent.
    /// Returns [`Error::LacksRights`] when the bot cannot manage topics.
    /// Returns [`Error::Transient`] or [`Error::Permanent`] for other failures.
    pub async fn create_topic(&self, title: String) -> Result<TopicId> {
        self.create_topic_with(title, TopicOptions::default()).await
    }

    /// Opens a forum topic and returns its `message_thread_id`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::create_topic`].
    pub async fn create_topic_with(&self, title: String, options: TopicOptions) -> Result<TopicId> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let title = title.clone();
                let options = options.clone();
                async move {
                    let params = CreateForumTopicParams::builder()
                        .chat_id(group_id)
                        .name(title)
                        .maybe_icon_color(options.icon_color.map(TopicIconColor::rgb))
                        .maybe_icon_custom_emoji_id(options.icon_custom_emoji_id)
                        .build();
                    api.create_forum_topic(&params).await
                }
            })
            .await?;
        Ok(TopicId(response.result.message_thread_id))
    }

    /// Closes a forum topic. The topic and its messages stay in the group.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TopicGone`] when the topic does not exist.
    /// Returns [`Error::RateLimited`], [`Error::LacksRights`], [`Error::Transient`],
    /// or [`Error::Permanent`] for the other Bot API outcomes.
    pub async fn close_topic(&self, topic: TopicId) -> Result<()> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = CloseForumTopicParams::builder()
                        .chat_id(group_id)
                        .message_thread_id(topic.0)
                        .build();
                    api.close_forum_topic(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Reopens a forum topic that was closed.
    ///
    /// # Errors
    ///
    /// Same as [`Self::close_topic`].
    pub async fn reopen_topic(&self, topic: TopicId) -> Result<()> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = ReopenForumTopicParams::builder()
                        .chat_id(group_id)
                        .message_thread_id(topic.0)
                        .build();
                    api.reopen_forum_topic(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Posts `Telegram` HTML into a topic.
    ///
    /// `reply_to` sets `reply_parameters` and allows the send when that message is already gone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TopicGone`] when the topic does not exist.
    /// Returns [`Error::RateLimited`] after the retry budget is spent.
    /// Returns [`Error::LacksRights`], [`Error::Transient`], or [`Error::Permanent`] otherwise.
    pub async fn send_text(
        &self,
        topic: TopicId,
        text_html: String,
        reply_to: Option<TgMessageId>,
    ) -> Result<TgMessageId> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let reply = reply_parameters(reply_to);
        let response = self
            .call(move || {
                let api = api.clone();
                let text_html = text_html.clone();
                let reply = reply.clone();
                async move { post_text(&api, group_id, topic, text_html, reply).await }
            })
            .await?;
        Ok(TgMessageId(response.result.message_id))
    }

    /// Posts one attachment into a topic.
    ///
    /// `caption` is plain text. Photos, videos, audio, voice, files, and GIFs use the matching
    /// Bot API method. A static sticker is sent as a photo. An animated or video sticker is sent
    /// as a document. A URL is passed through. A local path is uploaded as multipart.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TooLarge`] when the attachment is over the download or upload cap.
    /// Returns [`Error::TopicGone`] when the topic does not exist.
    /// Returns [`Error::Permanent`] for a kind that is not a file, or a path that cannot be read.
    /// Returns [`Error::RateLimited`], [`Error::LacksRights`], or [`Error::Transient`] for the
    /// other Bot API outcomes.
    pub async fn send_media(
        &self,
        topic: TopicId,
        attachment: Attachment,
        caption: Option<String>,
    ) -> Result<TgMessageId> {
        let pin = attachment.file_name.as_deref() == Some(limits::AVATAR_CARD_FILE);
        let method = media::media_method(&attachment)?;
        let upload = media::to_upload(media::prepare_file(&attachment).await?);
        let id = self.send_upload(topic, method, upload, caption).await?;
        if pin && let Err(error) = self.pin_message(id).await {
            tracing::warn!(topic = topic.0, message = id.0, %error, "avatar card pin failed");
        }
        Ok(id)
    }

    /// Renames a forum topic. The icon is left as it was.
    ///
    /// # Errors
    ///
    /// Same as [`Self::close_topic`].
    pub async fn edit_forum_topic(&self, topic: TopicId, title: String) -> Result<()> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let title = title.clone();
                async move {
                    let params = EditForumTopicParams::builder()
                        .chat_id(group_id)
                        .message_thread_id(topic.0)
                        .maybe_name(Some(title))
                        .build();
                    api.edit_forum_topic(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Pins a message without a notification.
    ///
    /// # Errors
    ///
    /// Same as [`Self::close_topic`].
    pub async fn pin_message(&self, message: TgMessageId) -> Result<()> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = PinChatMessageParams::builder()
                        .chat_id(group_id)
                        .message_id(message.0)
                        .maybe_disable_notification(Some(true))
                        .build();
                    api.pin_chat_message(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Posts photos as one or more media groups.
    ///
    /// A single photo uses `sendPhoto`. Two to [`ALBUM_MAX_ITEMS`] photos use `sendMediaGroup`.
    /// A longer list is split, and the caption is placed on the first photo only.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Permanent`] when `photos` is empty or an item is not a photo.
    /// Returns [`Error::TooLarge`] when any photo is over the cap. Nothing is sent in that case.
    /// Other errors match [`Self::send_media`]. A failure on a later batch leaves the earlier
    /// batches posted.
    pub async fn send_media_group(
        &self,
        topic: TopicId,
        photos: Vec<Attachment>,
        caption: Option<String>,
    ) -> Result<Vec<TgMessageId>> {
        if photos.is_empty() {
            return Err(Error::Permanent { status: None });
        }
        let mut prepared = Vec::with_capacity(photos.len());
        for photo in &photos {
            if photo.kind != hub_core::AttachmentKind::Photo {
                return Err(Error::Permanent { status: None });
            }
            prepared.push(media::prepare_file(photo).await?);
        }
        let mut ids = Vec::with_capacity(prepared.len());
        for (index, batch) in prepared.chunks(ALBUM_MAX_ITEMS).enumerate() {
            let batch_caption = if index == 0 { caption.clone() } else { None };
            if batch.len() == 1 {
                let id = self
                    .send_upload(
                        topic,
                        MediaMethod::Photo,
                        media::to_upload(batch[0].clone()),
                        batch_caption,
                    )
                    .await?;
                ids.push(id);
            } else {
                ids.extend(self.send_album(topic, batch, batch_caption).await?);
            }
        }
        Ok(ids)
    }

    /// Sets one emoji reaction, or clears reactions when `emoji` is `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TopicGone`] when `Telegram` reports the thread as gone.
    /// `setMessageReaction` itself does not take a thread id.
    /// Returns [`Error::RateLimited`], [`Error::LacksRights`], [`Error::Transient`],
    /// or [`Error::Permanent`] for the other outcomes.
    pub async fn set_reaction(
        &self,
        topic: TopicId,
        message: TgMessageId,
        emoji: Option<String>,
    ) -> Result<()> {
        let _ = topic;
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let emoji = emoji.clone();
                async move {
                    let reaction = match emoji {
                        Some(emoji) => vec![ReactionType::Emoji(
                            ReactionTypeEmoji::builder().emoji(emoji).build(),
                        )],
                        None => Vec::new(),
                    };
                    let params = SetMessageReactionParams::builder()
                        .chat_id(group_id)
                        .message_id(message.0)
                        .reaction(reaction)
                        .build();
                    api.set_message_reaction(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Deletes one message in the supergroup.
    ///
    /// # Errors
    ///
    /// Same classes as [`Self::set_reaction`]. The thread id is not sent.
    /// `Telegram` deletes by `chat_id` and `message_id`.
    pub async fn delete_message(&self, topic: TopicId, message: TgMessageId) -> Result<()> {
        let _ = topic;
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = DeleteMessageParams::builder()
                        .chat_id(group_id)
                        .message_id(message.0)
                        .build();
                    api.delete_message(&params).await
                }
            })
            .await?;
        truthy(response.result)
    }

    /// Replaces the HTML body of a text message.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Permanent`] when `Telegram` returns `false`.
    /// Returns [`Error::TopicGone`] when the description says the thread is gone.
    /// Returns [`Error::RateLimited`], [`Error::LacksRights`], or [`Error::Transient`] otherwise.
    pub async fn edit_message_text(&self, message: TgMessageId, text_html: String) -> Result<()> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let text_html = text_html.clone();
                async move {
                    let params = EditMessageTextParams::builder()
                        .chat_id(group_id)
                        .message_id(message.0)
                        .text(text_html)
                        .parse_mode(ParseMode::Html)
                        .build();
                    api.edit_message_text(&params).await
                }
            })
            .await?;
        match response.result {
            MessageOrBool::Bool(true) | MessageOrBool::Message(_) => Ok(()),
            MessageOrBool::Bool(false) => Err(Error::Permanent { status: None }),
        }
    }

    /// Checks the bot, the chat, and the bot's membership.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RateLimited`], [`Error::LacksRights`], [`Error::Transient`],
    /// or [`Error::Permanent`] when one of the three calls fails.
    /// A chat that fails the forum checks is still `Ok` with a failing verdict.
    pub async fn preflight(&self) -> Result<PreflightReport> {
        let me = self.fetch_me().await?;
        let chat = self.fetch_chat().await?;
        let member = self.fetch_member(me.id).await?;
        preflight::assemble(&me, &chat, &member)
    }

    /// Long-polls `getUpdates` and sends [`TopicIncoming`] values on `tx`.
    ///
    /// Messages from this bot and from other chats are skipped. The offset still advances
    /// past them. The loop returns when `shutdown` is set, when `tx` is closed, or when the
    /// offset sender is dropped.
    ///
    /// # Errors
    ///
    /// Returns the first `getMe`, `getUpdates`, or offset-store failure.
    pub async fn run_updates(
        &self,
        store: &impl OffsetStore,
        tx: mpsc::Sender<TopicIncoming>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let me = self.fetch_me().await?;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let offset = store.load().await?;
            let updates = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                    continue;
                }
                result = self.fetch_updates(offset) => result?,
            };
            for update in updates {
                for event in incoming::from_update(&update, self.group_id, me.id) {
                    if tx.send(event).await.is_err() {
                        return Ok(());
                    }
                }
                let next = i64::from(update.update_id) + 1;
                store.save(next).await?;
            }
        }
    }

    async fn send_upload(
        &self,
        topic: TopicId,
        method: MediaMethod,
        upload: FileUpload,
        caption: Option<String>,
    ) -> Result<TgMessageId> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let upload = upload.clone();
                let caption = caption.clone();
                async move { post_media(&api, group_id, topic, method, upload, caption).await }
            })
            .await?;
        Ok(TgMessageId(response.result.message_id))
    }

    async fn send_album(
        &self,
        topic: TopicId,
        batch: &[PreparedFile],
        caption: Option<String>,
    ) -> Result<Vec<TgMessageId>> {
        let files: Vec<PreparedFile> = batch.to_vec();
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                let files = files.clone();
                let caption = caption.clone();
                async move { post_album(&api, group_id, topic, &files, caption).await }
            })
            .await?;
        Ok(response
            .result
            .into_iter()
            .map(|message| TgMessageId(message.message_id))
            .collect())
    }

    async fn fetch_me(&self) -> Result<User> {
        let api = self.api.clone();
        let response = self
            .call(move || {
                let api = api.clone();
                async move { api.get_me().await }
            })
            .await?;
        Ok(response.result)
    }

    async fn fetch_chat(&self) -> Result<ChatFullInfo> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = GetChatParams::builder().chat_id(group_id).build();
                    api.get_chat(&params).await
                }
            })
            .await?;
        Ok(response.result)
    }

    async fn fetch_member(&self, user_id: u64) -> Result<ChatMember> {
        let api = self.api.clone();
        let group_id = self.group_id;
        let response = self
            .call(move || {
                let api = api.clone();
                async move {
                    let params = GetChatMemberParams::builder()
                        .chat_id(group_id)
                        .user_id(user_id)
                        .build();
                    api.get_chat_member(&params).await
                }
            })
            .await?;
        Ok(response.result)
    }

    async fn fetch_updates(&self, offset: Option<i64>) -> Result<Vec<Update>> {
        let api = self.poll_api.clone();
        let response = self
            .call_polling(move || {
                let api = api.clone();
                async move {
                    let params = GetUpdatesParams::builder()
                        .timeout(LONG_POLL_TIMEOUT_SECS)
                        .allowed_updates(vec![
                            AllowedUpdate::Message,
                            AllowedUpdate::MessageReaction,
                        ])
                        .maybe_offset(offset)
                        .build();
                    api.get_updates(&params).await
                }
            })
            .await?;
        Ok(response.result)
    }

    async fn call<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = std::result::Result<T, frankenstein::Error>> + Send,
    {
        self.queue.run(op).await
    }

    async fn icons_for(&self, title: &str) -> TopicOptions {
        let mut slot = self.icons.lock().await;
        if slot.is_none() {
            let set = match self.fetch_icon_stickers().await {
                Ok(stickers) => IconSet::from_stickers(&stickers),
                Err(error) => {
                    tracing::warn!(%error, "forum icon stickers unavailable");
                    IconSet::colors_only()
                }
            };
            *slot = Some(set);
        }
        match slot.as_ref() {
            Some(set) => set.for_title(title),
            None => IconSet::colors_only().for_title(title),
        }
    }

    async fn fetch_icon_stickers(&self) -> Result<Vec<Sticker>> {
        let api = self.api.clone();
        let response = self
            .call(move || {
                let api = api.clone();
                async move { api.get_forum_topic_icon_stickers().await }
            })
            .await?;
        Ok(response.result)
    }

    async fn call_polling<T, F, Fut>(&self, op: F) -> Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = std::result::Result<T, frankenstein::Error>> + Send,
    {
        self.queue.run_polling(op).await
    }
}

impl TopicSink for BotClient {
    async fn create_topic(&self, title: String) -> hub_core::Result<TopicId> {
        let options = self.icons_for(&title).await;
        self.create_topic_with(title, options)
            .await
            .map_err(Into::into)
    }

    async fn rename_topic(&self, topic: TopicId, title: String) -> hub_core::Result<()> {
        self.edit_forum_topic(topic, title)
            .await
            .map_err(Into::into)
    }

    async fn close_topic(&self, topic: TopicId) -> hub_core::Result<()> {
        BotClient::close_topic(self, topic)
            .await
            .map_err(Into::into)
    }

    async fn send_text(
        &self,
        topic: TopicId,
        text_html: String,
        reply_to: Option<TgMessageId>,
    ) -> hub_core::Result<TgMessageId> {
        BotClient::send_text(self, topic, text_html, reply_to)
            .await
            .map_err(Into::into)
    }

    async fn send_media(
        &self,
        topic: TopicId,
        attachment: Attachment,
        caption: Option<String>,
    ) -> hub_core::Result<TgMessageId> {
        BotClient::send_media(self, topic, attachment, caption)
            .await
            .map_err(Into::into)
    }

    async fn set_reaction(
        &self,
        topic: TopicId,
        message: TgMessageId,
        emoji: Option<String>,
    ) -> hub_core::Result<()> {
        BotClient::set_reaction(self, topic, message, emoji)
            .await
            .map_err(Into::into)
    }

    async fn delete_message(&self, topic: TopicId, message: TgMessageId) -> hub_core::Result<()> {
        BotClient::delete_message(self, topic, message)
            .await
            .map_err(Into::into)
    }
}

fn truthy(ok: bool) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Permanent { status: None })
    }
}

fn reply_parameters(reply_to: Option<TgMessageId>) -> Option<ReplyParameters> {
    reply_to.map(|message| {
        ReplyParameters::builder()
            .message_id(message.0)
            .allow_sending_without_reply(true)
            .build()
    })
}

async fn post_text(
    api: &ApiBot,
    group_id: i64,
    topic: TopicId,
    text_html: String,
    reply: Option<ReplyParameters>,
) -> std::result::Result<MethodResponse<Message>, frankenstein::Error> {
    let params = SendMessageParams::builder()
        .chat_id(group_id)
        .text(text_html)
        .message_thread_id(topic.0)
        .parse_mode(ParseMode::Html)
        .maybe_reply_parameters(reply)
        .build();
    api.send_message(&params).await
}

async fn post_media(
    api: &ApiBot,
    group_id: i64,
    topic: TopicId,
    method: MediaMethod,
    upload: FileUpload,
    caption: Option<String>,
) -> std::result::Result<MethodResponse<Message>, frankenstein::Error> {
    match method {
        MediaMethod::Photo => {
            let params = SendPhotoParams::builder()
                .chat_id(group_id)
                .photo(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_photo(&params).await
        }
        MediaMethod::Video => {
            let params = SendVideoParams::builder()
                .chat_id(group_id)
                .video(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_video(&params).await
        }
        MediaMethod::Audio => {
            let params = SendAudioParams::builder()
                .chat_id(group_id)
                .audio(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_audio(&params).await
        }
        MediaMethod::Voice => {
            let params = SendVoiceParams::builder()
                .chat_id(group_id)
                .voice(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_voice(&params).await
        }
        MediaMethod::Document => {
            let params = SendDocumentParams::builder()
                .chat_id(group_id)
                .document(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_document(&params).await
        }
        MediaMethod::Animation => {
            let params = SendAnimationParams::builder()
                .chat_id(group_id)
                .animation(upload)
                .message_thread_id(topic.0)
                .maybe_caption(caption)
                .build();
            api.send_animation(&params).await
        }
    }
}

async fn post_album(
    api: &ApiBot,
    group_id: i64,
    topic: TopicId,
    files: &[PreparedFile],
    caption: Option<String>,
) -> std::result::Result<MethodResponse<Vec<Message>>, frankenstein::Error> {
    let mut media = Vec::with_capacity(files.len());
    for (index, file) in files.iter().cloned().enumerate() {
        let item_caption = if index == 0 { caption.clone() } else { None };
        let item = InputMediaPhoto::builder()
            .media(media::to_upload(file))
            .maybe_caption(item_caption)
            .build();
        media.push(MediaGroupInputMedia::from(item));
    }
    let params = SendMediaGroupParams::builder()
        .chat_id(group_id)
        .media(media)
        .message_thread_id(topic.0)
        .build();
    api.send_media_group(&params).await
}
