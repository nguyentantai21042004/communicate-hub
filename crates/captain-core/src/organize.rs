//! Contacts, private history cleanup, and chat folder definitions shared across layers.

use serde::{Deserialize, Serialize};

use crate::limits::{CONTACT_NAME_MAX_UTF16, DIALOG_FILTER_ID_MIN, DIALOG_FILTER_TITLE_MAX_UTF16};
use crate::{ChatId, ChatRef, Error, Result};

/// One saved contact returned by Telegram's contact book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactInfo {
    /// Marked user id.
    pub id: ChatId,
    /// Saved first name.
    pub first_name: String,
    /// Saved last name.
    pub last_name: String,
    /// Public username without `@`, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Phone number recorded on the contact, if visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Whether both accounts have each other in contacts.
    pub mutual: bool,
}

/// Request to add or rename one user in our own Telegram contact list without notifying them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactUpsert {
    /// Target user.
    pub user: ChatRef,
    /// First name to store in our address book.
    pub first_name: String,
    /// Last name to store in our address book.
    #[serde(default)]
    pub last_name: String,
    /// Explicit phone override; `None` preserves any existing saved phone or sends `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
}

impl ContactUpsert {
    /// Checks non-empty name and Telegram field lengths before any request is sent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyField`] if both names are blank, or [`Error::TooLong`] when a name
    /// exceeds [`CONTACT_NAME_MAX_UTF16`].
    pub fn validate(&self) -> Result<()> {
        if self.first_name.trim().is_empty() && self.last_name.trim().is_empty() {
            return Err(Error::EmptyField("contact name"));
        }
        check_max_utf16("first_name", &self.first_name, CONTACT_NAME_MAX_UTF16)?;
        check_max_utf16("last_name", &self.last_name, CONTACT_NAME_MAX_UTF16)?;
        Ok(())
    }
}

/// Result of deleting our own local copy of a private chat history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedHistory {
    /// Marked id of the private chat.
    pub chat: ChatId,
    /// Total server `pts_count` acknowledged across deletion pages.
    pub affected_count: usize,
}

/// Kind of chat folder in Telegram's top filter bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogFolderKind {
    /// Built-in All Chats tab (`id = 0`).
    Default,
    /// Standard custom filter folder (`dialogFilter`).
    Filter,
    /// Shareable chatlist folder (`dialogFilterChatlist`).
    Chatlist,
}

/// Current account folder configuration returned by Telegram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogFolders {
    /// Whether folder tags are enabled on chats.
    pub tags_enabled: bool,
    /// Folders in their display order.
    pub folders: Vec<DialogFolder>,
}

/// One Telegram chat folder in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogFolder {
    /// Server folder id (`0` for Default, `>= 2` for custom folders).
    pub id: i32,
    /// Folder constructor kind.
    pub kind: DialogFolderKind,
    /// Title shown on the tab bar.
    pub title: String,
    /// Includes all 1-on-1 contacts.
    pub contacts: bool,
    /// Includes all 1-on-1 non-contacts.
    pub non_contacts: bool,
    /// Includes all groups and supergroups.
    pub groups: bool,
    /// Includes all channels.
    pub broadcasts: bool,
    /// Includes all bots.
    pub bots: bool,
    /// Excludes muted chats.
    pub exclude_muted: bool,
    /// Excludes already-read chats.
    pub exclude_read: bool,
    /// Excludes archived chats.
    pub exclude_archived: bool,
    /// Optional tab icon identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoticon: Option<String>,
    /// Optional folder tag color index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<i32>,
    /// Explicitly pinned chat ids.
    pub pinned_peers: Vec<ChatId>,
    /// Explicitly included chat ids.
    pub include_peers: Vec<ChatId>,
    /// Explicitly excluded chat ids.
    pub exclude_peers: Vec<ChatId>,
}

/// Specification to create or replace a custom chat folder (`dialogFilter`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogFolderSpec {
    /// Folder id, at least [`DIALOG_FILTER_ID_MIN`].
    pub id: i32,
    /// Tab title shown on Telegram (`<=` [`DIALOG_FILTER_TITLE_MAX_UTF16`] UTF-16 units).
    pub title: String,
    /// Include all 1-on-1 contacts.
    #[serde(default)]
    pub contacts: bool,
    /// Include all 1-on-1 non-contacts.
    #[serde(default)]
    pub non_contacts: bool,
    /// Include all groups and supergroups.
    #[serde(default)]
    pub groups: bool,
    /// Include all broadcast channels.
    #[serde(default)]
    pub broadcasts: bool,
    /// Include all bots.
    #[serde(default)]
    pub bots: bool,
    /// Exclude muted chats.
    #[serde(default)]
    pub exclude_muted: bool,
    /// Exclude read chats.
    #[serde(default)]
    pub exclude_read: bool,
    /// Exclude archived chats.
    #[serde(default)]
    pub exclude_archived: bool,
    /// Disable animated custom emoji in title.
    #[serde(default)]
    pub title_noanimate: bool,
    /// Optional folder icon string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoticon: Option<String>,
    /// Optional folder tag color index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<i32>,
    /// Pinned chats in this folder.
    #[serde(default)]
    pub pinned_peers: Vec<ChatRef>,
    /// Explicitly included chats.
    #[serde(default)]
    pub include_peers: Vec<ChatRef>,
    /// Explicitly excluded chats.
    #[serde(default)]
    pub exclude_peers: Vec<ChatRef>,
}

impl DialogFolderSpec {
    /// Checks folder id and title constraints before resolving peers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFolderId`], [`Error::EmptyField`], or [`Error::TooLong`].
    pub fn validate(&self) -> Result<()> {
        if self.id < DIALOG_FILTER_ID_MIN {
            return Err(Error::InvalidFolderId {
                id: self.id,
                min: DIALOG_FILTER_ID_MIN,
            });
        }
        if self.title.trim().is_empty() {
            return Err(Error::EmptyField("folder title"));
        }
        check_max_utf16("folder title", &self.title, DIALOG_FILTER_TITLE_MAX_UTF16)?;
        Ok(())
    }
}

fn check_max_utf16(what: &'static str, value: &str, max_utf16: usize) -> Result<()> {
    let len_utf16 = value.encode_utf16().count();
    if len_utf16 > max_utf16 {
        return Err(Error::TooLong {
            what,
            len_utf16,
            max_utf16,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_contact_upsert_bounds() {
        let valid = ContactUpsert {
            user: ChatRef::Id(ChatId(123)),
            first_name: "[TEAM] Alice".into(),
            last_name: String::new(),
            phone: None,
        };
        assert!(valid.validate().is_ok());

        let blank = ContactUpsert {
            user: ChatRef::Id(ChatId(123)),
            first_name: "   ".into(),
            last_name: String::new(),
            phone: None,
        };
        assert_eq!(blank.validate(), Err(Error::EmptyField("contact name")));

        let too_long = ContactUpsert {
            user: ChatRef::Id(ChatId(123)),
            first_name: "a".repeat(CONTACT_NAME_MAX_UTF16 + 1),
            last_name: String::new(),
            phone: None,
        };
        assert!(matches!(too_long.validate(), Err(Error::TooLong { .. })));
    }

    #[test]
    fn validates_folder_spec_bounds() {
        let valid = DialogFolderSpec {
            id: 2,
            title: "Tech & Ops".into(),
            contacts: false,
            non_contacts: false,
            groups: false,
            broadcasts: false,
            bots: false,
            exclude_muted: false,
            exclude_read: false,
            exclude_archived: false,
            title_noanimate: false,
            emoticon: None,
            color: None,
            pinned_peers: Vec::new(),
            include_peers: vec![ChatRef::Id(ChatId(-100))],
            exclude_peers: Vec::new(),
        };
        assert!(valid.validate().is_ok());

        let bad_id = DialogFolderSpec {
            id: 1,
            ..valid.clone()
        };
        assert_eq!(
            bad_id.validate(),
            Err(Error::InvalidFolderId {
                id: 1,
                min: DIALOG_FILTER_ID_MIN
            })
        );

        let too_long = DialogFolderSpec {
            title: "1234567890123".into(),
            ..valid
        };
        assert!(matches!(too_long.validate(), Err(Error::TooLong { .. })));
    }
}
