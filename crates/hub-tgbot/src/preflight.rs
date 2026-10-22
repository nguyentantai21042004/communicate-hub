//! `checktopics` report: supergroup, forum mode, and the bot's rights.
//!
//! The dashboard can show [`PreflightReport`] as it is. A chat that is not a
//! forum supergroup fails before the rights list, because those rights are
//! meaningless there.

use frankenstein::types::{ChatFullInfo, ChatMember, ChatType, User};

use crate::error::{Error, Result};

/// One right the bridge needs in the supergroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotRight {
    /// `can_manage_topics`, or creator.
    ManageTopics,
    /// The bot can post.
    SendMessages,
    /// The bot can delete posts.
    DeleteMessages,
    /// The chat allows reactions and the member is allowed to react.
    React,
}

impl BotRight {
    fn as_str(self) -> &'static str {
        match self {
            Self::ManageTopics => "manage topics",
            Self::SendMessages => "send messages",
            Self::DeleteMessages => "delete messages",
            Self::React => "react",
        }
    }
}

impl std::fmt::Display for BotRight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What `getMe`, `getChat`, and `getChatMember` said about the bridge group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    /// Bot user id from `getMe`.
    pub bot_id: i64,
    /// Bot username, when `Telegram` sent one.
    pub bot_username: Option<String>,
    /// `true` when `getChat` reports `type` `supergroup`.
    pub is_supergroup: bool,
    /// `true` when forum topics are enabled.
    pub is_forum: bool,
    /// The bot may create and close topics.
    pub can_manage_topics: bool,
    /// The bot may post into the group.
    pub can_send_messages: bool,
    /// The bot may delete messages.
    pub can_delete_messages: bool,
    /// The chat has at least one allowed reaction and the bot may use it.
    pub can_react: bool,
    /// The single verdict a dashboard should show first.
    pub verdict: PreflightVerdict,
}

/// Why preflight passed or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightVerdict {
    /// The group is a forum supergroup and the bot has every right the bridge uses.
    Ready,
    /// `getChat` did not report a supergroup.
    NotSupergroup,
    /// The chat is a supergroup and topics are off.
    ForumDisabled,
    /// Topics are on and at least one right is missing.
    MissingRights(Vec<BotRight>),
}

pub(crate) fn assemble(
    me: &User,
    chat: &ChatFullInfo,
    member: &ChatMember,
) -> Result<PreflightReport> {
    let bot_id = i64::try_from(me.id).map_err(|_| Error::Permanent { status: None })?;
    let is_supergroup = chat.type_field == ChatType::Supergroup;
    let is_forum = chat.is_forum == Some(true);
    let reactions_enabled = match &chat.available_reactions {
        Some(reactions) => !reactions.is_empty(),
        // TELEGRAM: omitting available_reactions means the chat still allows the default set.
        None => true,
    };
    let (can_manage_topics, can_send_messages, can_delete_messages, member_can_react) =
        member_rights(member);
    let can_react = reactions_enabled && member_can_react;
    let mut missing = Vec::new();
    if !can_manage_topics {
        missing.push(BotRight::ManageTopics);
    }
    if !can_send_messages {
        missing.push(BotRight::SendMessages);
    }
    if !can_delete_messages {
        missing.push(BotRight::DeleteMessages);
    }
    if !can_react {
        missing.push(BotRight::React);
    }
    let verdict = if !is_supergroup {
        PreflightVerdict::NotSupergroup
    } else if !is_forum {
        PreflightVerdict::ForumDisabled
    } else if missing.is_empty() {
        PreflightVerdict::Ready
    } else {
        PreflightVerdict::MissingRights(missing)
    };
    Ok(PreflightReport {
        bot_id,
        bot_username: me.username.clone(),
        is_supergroup,
        is_forum,
        can_manage_topics,
        can_send_messages,
        can_delete_messages,
        can_react,
        verdict,
    })
}

fn member_rights(member: &ChatMember) -> (bool, bool, bool, bool) {
    match member {
        ChatMember::Creator(_) => (true, true, true, true),
        ChatMember::Administrator(admin) => (
            admin.can_manage_topics == Some(true),
            true,
            admin.can_delete_messages,
            true,
        ),
        ChatMember::Member(_) => (false, true, false, true),
        ChatMember::Restricted(restricted) => (
            restricted.can_manage_topics,
            restricted.can_send_messages,
            false,
            restricted.can_react_to_messages,
        ),
        ChatMember::Left(_) | ChatMember::Kicked(_) => (false, false, false, false),
    }
}
