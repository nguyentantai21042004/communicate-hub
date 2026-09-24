//! Trigger rules: whether an in-scope message should make someone look at its thread.

use captain_core::MessageView;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::config::{ChatEntry, EntryKind};
use crate::ticket::str_enum;

str_enum! {
    /// Which rule fired.
    pub enum TriggerKind {
        /// Any incoming message in a scoped private chat.
        DmAll => "dm_all",
        /// The message mentions the account.
        Mention => "mention",
        /// The message replies to one of the account's messages.
        ReplyToMe => "reply_to_me",
        /// The text matches a keyword of the chat.
        Keyword => "keyword",
        /// The owner asked for it with a `cap:` note or the CLI.
        Manual => "manual",
    }
}

/// A fired trigger and a short human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// Rule that fired.
    pub kind: TriggerKind,
    /// For example `mentioned in message 812`. Never contains message text.
    pub reason: String,
}

/// Returns the first trigger `message` fires in `entry`, in the order private chat, mention,
/// reply, keyword.
///
/// `reply_target_outgoing` says whether the message it replies to was sent by the account, or
/// `None` when that is unknown or it is not a reply. The caller filters out the account's own
/// messages and bots before calling.
pub fn detect(
    message: &MessageView,
    entry: &ChatEntry,
    reply_target_outgoing: Option<bool>,
) -> Option<Trigger> {
    let fire = |kind, reason: String| Some(Trigger { kind, reason });
    if entry.kind == EntryKind::Dm && entry.enables(TriggerKind::DmAll) {
        return fire(
            TriggerKind::DmAll,
            format!("private message {}", message.id.0),
        );
    }
    if message.mentioned && entry.enables(TriggerKind::Mention) {
        return fire(
            TriggerKind::Mention,
            format!("mentioned in message {}", message.id.0),
        );
    }
    if reply_target_outgoing == Some(true)
        && entry.enables(TriggerKind::ReplyToMe)
        && let Some(target) = message.reply_to
    {
        return fire(
            TriggerKind::ReplyToMe,
            format!("reply to your message {}", target.0),
        );
    }
    if entry.enables(TriggerKind::Keyword) && !entry.keywords.is_empty() {
        let text = normalize(&message.text.text);
        if let Some(keyword) = entry.keywords.iter().find(|k| text.contains(k.as_str())) {
            return fire(TriggerKind::Keyword, format!("keyword {keyword:?}"));
        }
    }
    None
}

/// Lowercases and strips diacritics, so `Hoàn tiền` and `hoan tien` compare equal.
///
/// `đ` has no decomposition in Unicode, so it is mapped to `d` by hand.
pub fn normalize(text: &str) -> String {
    text.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c == 'đ' { 'd' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_entry;
    use captain_core::{ChatId, ChatKind, ChatSummary, FormattedText, MessageId};

    fn message(text: &str) -> MessageView {
        MessageView {
            chat: ChatSummary {
                id: ChatId(-100),
                kind: ChatKind::Channel,
                name: None,
                username: None,
                is_bot: false,
            },
            id: MessageId(10),
            sender: None,
            outgoing: false,
            date_unix: 0,
            text: FormattedText::plain(text),
            media: None,
            reply_to: None,
            album_id: None,
            mentioned: false,
            topic: None,
            quote_text: None,
            forward: None,
            edited_at_unix: None,
        }
    }

    #[test]
    fn keyword_ignores_case_and_diacritics() {
        let entry = test_entry(EntryKind::Group, &[TriggerKind::Keyword], &["hoàn tiền"]);
        let hit = detect(&message("Khách đòi HOAN TIEN gấp"), &entry, None).unwrap();
        assert_eq!(hit.kind, TriggerKind::Keyword);
        assert!(detect(&message("không liên quan"), &entry, None).is_none());
        assert_eq!(normalize("Đặt đơn"), "dat don");
    }

    #[test]
    fn mention_beats_keyword_and_needs_to_be_enabled() {
        let mut m = message("deploy xong chưa");
        m.mentioned = true;
        let both = test_entry(
            EntryKind::Group,
            &[TriggerKind::Mention, TriggerKind::Keyword],
            &["deploy"],
        );
        assert_eq!(detect(&m, &both, None).unwrap().kind, TriggerKind::Mention);
        let keyword_only = test_entry(EntryKind::Group, &[TriggerKind::Keyword], &["deploy"]);
        assert_eq!(
            detect(&m, &keyword_only, None).unwrap().kind,
            TriggerKind::Keyword
        );
    }

    #[test]
    fn reply_to_me_needs_a_known_outgoing_target() {
        let mut m = message("ok");
        m.reply_to = Some(MessageId(3));
        let entry = test_entry(EntryKind::Group, &[TriggerKind::ReplyToMe], &[]);
        assert!(detect(&m, &entry, None).is_none());
        assert!(detect(&m, &entry, Some(false)).is_none());
        assert_eq!(
            detect(&m, &entry, Some(true)).unwrap().kind,
            TriggerKind::ReplyToMe
        );
    }

    #[test]
    fn every_dm_message_fires() {
        let entry = test_entry(EntryKind::Dm, &[TriggerKind::DmAll], &[]);
        assert_eq!(
            detect(&message("hi"), &entry, None).unwrap().kind,
            TriggerKind::DmAll
        );
    }
}
