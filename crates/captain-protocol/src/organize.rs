//! Contacts, private DM cleanup, and chat folder operations over MTProto.
//!
//! Sends zero notifications to remote users: contact edits disable phone privacy exceptions, and
//! history deletion always sets `revoke: false` on user peers only.

use std::collections::HashMap;

use captain_core::{
    ChatId, ChatKind, ChatRef, ChatSummary, ContactInfo, ContactUpsert, DeletedHistory,
    DialogFolder, DialogFolderKind, DialogFolderSpec, DialogFolders, Error as CoreError,
};
use grammers_client::Client;
use grammers_client::session::types::{PeerId, PeerKind};
use grammers_client::tl;

use crate::error::rpc;
use crate::inbound::chat_id;
use crate::outbound::Outbound;
use crate::{Error, Result};

const DELETE_HISTORY_MAX_PAGES: usize = 200; // cap loop if server repeatedly returns offset > 0

pub(crate) async fn list_contacts(client: &Client, self_id: i64) -> Result<Vec<ContactInfo>> {
    let raw = client
        .invoke(&tl::functions::contacts::GetContacts { hash: 0 })
        .await
        .map_err(rpc("contacts.getContacts"))?;
    let tl::enums::contacts::Contacts::Contacts(data) = raw else {
        return Ok(Vec::new());
    };
    let mutual_by_id: HashMap<i64, bool> = data
        .contacts
        .into_iter()
        .map(|tl::enums::Contact::Contact(c)| (c.user_id, c.mutual))
        .collect();
    let mut out = Vec::new();
    for user in data.users {
        let tl::enums::User::User(u) = user else {
            continue;
        };
        if u.deleted {
            continue;
        }
        let id = PeerId::user(u.id).map_or(ChatId(u.id), |p| chat_id(p, self_id));
        out.push(ContactInfo {
            id,
            first_name: u.first_name.unwrap_or_default(),
            last_name: u.last_name.unwrap_or_default(),
            username: u.username,
            phone: u.phone,
            mutual: mutual_by_id.get(&u.id).copied().unwrap_or(u.mutual_contact),
        });
    }
    Ok(out)
}

pub(crate) async fn upsert_contact(outbound: &Outbound<'_>, spec: ContactUpsert) -> Result<()> {
    spec.validate()?;
    let peer = outbound.resolve(&spec.user).await?;
    if peer.id.kind() != PeerKind::User {
        return Err(Error::Invalid(CoreError::InvalidChatRef(format!(
            "contact target {:?} is not a user",
            spec.user
        ))));
    }
    let user_id = peer.id.bare_id().unwrap_or(outbound.self_id);
    let phone = match spec.phone {
        Some(phone) => phone,
        None => existing_phone(outbound.client, user_id).await?,
    };
    // TELEGRAM: add_phone_privacy_exception stays false so renaming or adding a contact sends no
    // notification and does not grant a phone visibility exception.
    outbound
        .client
        .invoke(&tl::functions::contacts::AddContact {
            add_phone_privacy_exception: false,
            id: tl::enums::InputUser::from(&peer),
            first_name: spec.first_name,
            last_name: spec.last_name,
            phone,
            note: None,
        })
        .await
        .map_err(rpc("contacts.addContact"))?;
    Ok(())
}

async fn existing_phone(client: &Client, user_id: i64) -> Result<String> {
    let raw = client
        .invoke(&tl::functions::contacts::GetContacts { hash: 0 })
        .await
        .map_err(rpc("contacts.getContacts"))?;
    let tl::enums::contacts::Contacts::Contacts(data) = raw else {
        return Ok(String::new());
    };
    Ok(data
        .users
        .into_iter()
        .find_map(|user| match user {
            tl::enums::User::User(u) if u.id == user_id => u.phone,
            _ => None,
        })
        .unwrap_or_default())
}

pub(crate) async fn delete_user_history(
    outbound: &Outbound<'_>,
    chat: &ChatRef,
    just_clear: bool,
) -> Result<DeletedHistory> {
    let peer = outbound.resolve(chat).await?;
    if peer.id.kind() != PeerKind::User {
        return Err(Error::Invalid(CoreError::InvalidChatRef(format!(
            "history delete target {chat:?} is not a private user DM"
        ))));
    }
    let chat_id = chat_id(peer.id, outbound.self_id);
    let input_peer = tl::enums::InputPeer::from(&peer);
    let mut affected_count = 0usize;
    for _ in 0..DELETE_HISTORY_MAX_PAGES {
        // TELEGRAM: revoke stays false ("delete just for me") so the other participant keeps
        // their copy and receives no notification.
        let tl::enums::messages::AffectedHistory::History(res) = outbound
            .client
            .invoke(&tl::functions::messages::DeleteHistory {
                just_clear,
                revoke: false,
                peer: input_peer.clone(),
                max_id: 0,
                min_date: None,
                max_date: None,
            })
            .await
            .map_err(rpc("messages.deleteHistory"))?;
        affected_count += usize::try_from(res.pts_count).unwrap_or(0);
        if res.offset == 0 {
            break;
        }
    }
    Ok(DeletedHistory {
        chat: chat_id,
        affected_count,
    })
}

pub(crate) async fn dialog_folders(client: &Client, self_id: i64) -> Result<DialogFolders> {
    let tl::enums::messages::DialogFilters::Filters(raw) = client
        .invoke(&tl::functions::messages::GetDialogFilters {})
        .await
        .map_err(rpc("messages.getDialogFilters"))?;
    let folders = raw
        .filters
        .into_iter()
        .map(|filter| folder_from_tl(filter, self_id))
        .collect();
    Ok(DialogFolders {
        tags_enabled: raw.tags_enabled,
        folders,
    })
}

pub(crate) async fn upsert_dialog_folder(
    outbound: &Outbound<'_>,
    spec: DialogFolderSpec,
) -> Result<()> {
    spec.validate()?;
    let pinned_peers = resolve_input_peers(outbound, &spec.pinned_peers).await?;
    let include_peers = resolve_input_peers(outbound, &spec.include_peers).await?;
    let exclude_peers = resolve_input_peers(outbound, &spec.exclude_peers).await?;
    let filter = tl::enums::DialogFilter::Filter(tl::types::DialogFilter {
        contacts: spec.contacts,
        non_contacts: spec.non_contacts,
        groups: spec.groups,
        broadcasts: spec.broadcasts,
        bots: spec.bots,
        exclude_muted: spec.exclude_muted,
        exclude_read: spec.exclude_read,
        exclude_archived: spec.exclude_archived,
        title_noanimate: spec.title_noanimate,
        id: spec.id,
        title: tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
            text: spec.title,
            entities: Vec::new(),
        }),
        emoticon: spec.emoticon,
        color: spec.color,
        pinned_peers,
        include_peers,
        exclude_peers,
    });
    outbound
        .client
        .invoke(&tl::functions::messages::UpdateDialogFilter {
            id: spec.id,
            filter: Some(filter),
        })
        .await
        .map_err(rpc("messages.updateDialogFilter"))?;
    Ok(())
}

pub(crate) async fn delete_dialog_folder(client: &Client, id: i32) -> Result<()> {
    if id < captain_core::limits::DIALOG_FILTER_ID_MIN {
        return Err(Error::Invalid(CoreError::InvalidFolderId {
            id,
            min: captain_core::limits::DIALOG_FILTER_ID_MIN,
        }));
    }
    client
        .invoke(&tl::functions::messages::UpdateDialogFilter { id, filter: None })
        .await
        .map_err(rpc("messages.updateDialogFilter"))?;
    Ok(())
}

pub(crate) async fn reorder_dialog_folders(client: &Client, order: Vec<i32>) -> Result<()> {
    client
        .invoke(&tl::functions::messages::UpdateDialogFiltersOrder { order })
        .await
        .map_err(rpc("messages.updateDialogFiltersOrder"))?;
    Ok(())
}

pub(crate) async fn create_private_group(
    client: &Client,
    self_id: i64,
    title: String,
    about: String,
) -> Result<ChatSummary> {
    if title.trim().is_empty() {
        return Err(Error::Invalid(CoreError::EmptyField("group title")));
    }
    let updates = client
        .invoke(&tl::functions::channels::CreateChannel {
            broadcast: false,
            megagroup: true,
            for_import: false,
            forum: false,
            title: title.clone(),
            about,
            geo_point: None,
            address: None,
            ttl_period: None,
        })
        .await
        .map_err(rpc("channels.createChannel"))?;
    let chats = match updates {
        tl::enums::Updates::Updates(u) => u.chats,
        tl::enums::Updates::Combined(u) => u.chats,
        _ => Vec::new(),
    };
    let created = chats.into_iter().find_map(|chat| match chat {
        tl::enums::Chat::Channel(ch) => Some(ch),
        _ => None,
    });
    let Some(channel) = created else {
        return Err(Error::Unexpected {
            request: "channels.createChannel",
            detail: "response contained no created channel".to_owned(),
        });
    };
    // Warm the peer cache so immediate sends to the new group resolve its access hash.
    let mut dialogs = client.iter_dialogs().limit(10);
    while dialogs
        .next()
        .await
        .map_err(rpc("messages.getDialogs"))?
        .is_some()
    {}
    let id = PeerId::channel(channel.id).map_or(ChatId(-1_000_000_000_000 - channel.id), |p| {
        chat_id(p, self_id)
    });
    Ok(ChatSummary {
        id,
        kind: ChatKind::Channel,
        name: Some(channel.title),
        username: channel.username,
        is_bot: false,
    })
}

async fn resolve_input_peers(
    outbound: &Outbound<'_>,
    refs: &[ChatRef],
) -> Result<Vec<tl::enums::InputPeer>> {
    let mut out = Vec::with_capacity(refs.len());
    for chat in refs {
        let peer = outbound.resolve(chat).await?;
        out.push(tl::enums::InputPeer::from(&peer));
    }
    Ok(out)
}

fn folder_from_tl(filter: tl::enums::DialogFilter, self_id: i64) -> DialogFolder {
    match filter {
        tl::enums::DialogFilter::Default => DialogFolder {
            id: 0,
            kind: DialogFolderKind::Default,
            title: "All".to_owned(),
            contacts: false,
            non_contacts: false,
            groups: false,
            broadcasts: false,
            bots: false,
            exclude_muted: false,
            exclude_read: false,
            exclude_archived: false,
            emoticon: None,
            color: None,
            pinned_peers: Vec::new(),
            include_peers: Vec::new(),
            exclude_peers: Vec::new(),
        },
        tl::enums::DialogFilter::Filter(f) => {
            let tl::enums::TextWithEntities::Entities(title) = f.title;
            DialogFolder {
                id: f.id,
                kind: DialogFolderKind::Filter,
                title: title.text,
                contacts: f.contacts,
                non_contacts: f.non_contacts,
                groups: f.groups,
                broadcasts: f.broadcasts,
                bots: f.bots,
                exclude_muted: f.exclude_muted,
                exclude_read: f.exclude_read,
                exclude_archived: f.exclude_archived,
                emoticon: f.emoticon,
                color: f.color,
                pinned_peers: peers_to_ids(&f.pinned_peers, self_id),
                include_peers: peers_to_ids(&f.include_peers, self_id),
                exclude_peers: peers_to_ids(&f.exclude_peers, self_id),
            }
        }
        tl::enums::DialogFilter::Chatlist(f) => {
            let tl::enums::TextWithEntities::Entities(title) = f.title;
            DialogFolder {
                id: f.id,
                kind: DialogFolderKind::Chatlist,
                title: title.text,
                contacts: false,
                non_contacts: false,
                groups: false,
                broadcasts: false,
                bots: false,
                exclude_muted: false,
                exclude_read: false,
                exclude_archived: false,
                emoticon: f.emoticon,
                color: f.color,
                pinned_peers: peers_to_ids(&f.pinned_peers, self_id),
                include_peers: peers_to_ids(&f.include_peers, self_id),
                exclude_peers: Vec::new(),
            }
        }
    }
}

fn peers_to_ids(peers: &[tl::enums::InputPeer], self_id: i64) -> Vec<ChatId> {
    peers
        .iter()
        .filter_map(|p| input_peer_chat_id(p, self_id))
        .collect()
}

fn input_peer_chat_id(peer: &tl::enums::InputPeer, self_id: i64) -> Option<ChatId> {
    match peer {
        tl::enums::InputPeer::PeerSelf => Some(ChatId(self_id)),
        tl::enums::InputPeer::User(u) => PeerId::user(u.user_id).map(|p| chat_id(p, self_id)),
        tl::enums::InputPeer::UserFromMessage(u) => {
            PeerId::user(u.user_id).map(|p| chat_id(p, self_id))
        }
        tl::enums::InputPeer::Chat(c) => PeerId::chat(c.chat_id).map(|p| chat_id(p, self_id)),
        tl::enums::InputPeer::Channel(c) => {
            PeerId::channel(c.channel_id).map(|p| chat_id(p, self_id))
        }
        tl::enums::InputPeer::ChannelFromMessage(c) => {
            PeerId::channel(c.channel_id).map(|p| chat_id(p, self_id))
        }
        tl::enums::InputPeer::Empty => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_input_peer_variants_to_marked_ids() {
        let user = tl::enums::InputPeer::User(tl::types::InputPeerUser {
            user_id: 42,
            access_hash: 99,
        });
        let chat = tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: 55 });
        let channel = tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
            channel_id: 77,
            access_hash: 88,
        });
        assert_eq!(input_peer_chat_id(&user, 1), Some(ChatId(42)));
        assert_eq!(input_peer_chat_id(&chat, 1), Some(ChatId(-55)));
        assert_eq!(
            input_peer_chat_id(&channel, 1),
            Some(ChatId(-1_000_000_000_077))
        );
        assert_eq!(
            input_peer_chat_id(&tl::enums::InputPeer::PeerSelf, 1),
            Some(ChatId(1))
        );
        assert_eq!(input_peer_chat_id(&tl::enums::InputPeer::Empty, 1), None);
    }
}
