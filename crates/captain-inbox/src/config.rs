//! `scope.toml` and `policy.toml`: which chats are watched, and the numbers that tune rules.
//!
//! Both files are rejected whole on any error, so a typo never silently widens or narrows
//! scope. Formats are described in `docs/design/scope.md`.

use std::collections::{BTreeMap, HashMap, HashSet};

use captain_core::{ChatId, MessageId};
use serde::{Deserialize, Serialize};

use crate::ticket::ThreadKey;
use crate::trigger::{TriggerKind, normalize};
use crate::{Error, Result, parse_date};

const SCOPE_FILE: &str = "scope.toml";
const POLICY_FILE: &str = "policy.toml";
const FILE_LEVEL: i64 = 0; // chat id reported for problems not tied to one entry
/// Forum topic id of the General topic. Its messages carry no topic in their reply header.
pub const GENERAL_TOPIC: MessageId = MessageId(1);

/// Profile names every policy has, even when `policy.toml` is absent.
pub const DM_PROFILE: &str = "dm";
/// Default profile for groups and forums.
pub const GROUP_PROFILE: &str = "group";

const DM_DEBOUNCE_SECS: i64 = 45; // people send "anh ơi", then the question
const DM_BURST_MAX_SECS: i64 = 300;
const DM_BURST_MAX_MESSAGES: usize = 30;
const GROUP_DEBOUNCE_SECS: i64 = 10;
const GROUP_BURST_MAX_SECS: i64 = 120;
const GROUP_BURST_MAX_MESSAGES: usize = 20;
const RESOLVE_WINDOW_SECS: i64 = 24 * 3600;
const STALE_AFTER_SECS: i64 = 6 * 3600;
const MESSAGE_RETENTION_DAYS: i64 = 30;
const METRIC_RETENTION_DAYS: i64 = 180;
const MANUAL_BEFORE_SECS: i64 = 120; // forwards sent this long before a `cap:` note join it
const MANUAL_AFTER_SECS: i64 = 10; // and forwards sent this long after it

/// Kind of a scope entry, which decides the triggers it may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Private chat with one person.
    Dm,
    /// Basic group or supergroup without topics.
    Group,
    /// Supergroup with forum topics.
    Forum,
}

/// One watched chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatEntry {
    /// Marked chat id.
    pub id: ChatId,
    /// Name shown in reports, chosen by the owner.
    pub label: String,
    /// Kind of chat.
    pub kind: EntryKind,
    /// Watched forum topics; `None` watches all of them.
    pub topics: Option<Vec<MessageId>>,
    /// Enabled triggers.
    pub triggers: Vec<TriggerKind>,
    /// Keywords, already normalized with [`normalize`].
    pub keywords: Vec<String>,
    /// Policy profile name.
    pub profile: String,
    /// No tickets until this Unix time.
    pub mute_until_unix: Option<i64>,
}

impl ChatEntry {
    /// Whether this entry enables `kind`.
    pub fn enables(&self, kind: TriggerKind) -> bool {
        self.triggers.contains(&kind)
    }

    /// Whether the entry is muted at `now`.
    pub fn muted(&self, now: i64) -> bool {
        self.mute_until_unix.is_some_and(|until| now < until)
    }
}

/// The parsed scope list. Chats not in it are out of scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Scope {
    entries: HashMap<ChatId, ChatEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeFile {
    #[serde(default)]
    chat: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    id: i64,
    label: String,
    kind: EntryKind,
    #[serde(default)]
    topics: Option<Vec<i32>>,
    triggers: Vec<TriggerKind>,
    #[serde(default)]
    keywords: Vec<String>,
    profile: Option<String>,
    #[serde(default)]
    mute_until: Option<String>,
}

impl Scope {
    /// Parses `scope.toml` text, checking profile names against `policy`.
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] for invalid TOML or unknown keys, and [`Error::Invalid`] for a duplicate
    /// id, an unknown profile, a trigger the chat kind cannot use, keywords missing for the
    /// keyword trigger, or topics on a chat that is not a forum.
    pub fn parse(text: &str, policy: &Policy) -> Result<Self> {
        let file: ScopeFile = toml::from_str(text).map_err(|e| Error::Parse {
            file: SCOPE_FILE,
            reason: e.message().to_owned(),
        })?;
        let mut entries = HashMap::new();
        for raw in file.chat {
            let entry = validate(raw, policy)?;
            let id = entry.id;
            if entries.insert(id, entry).is_some() {
                return Err(invalid(id.0, "listed twice"));
            }
        }
        Ok(Self { entries })
    }

    /// The entry covering a message on `key`, if the chat and topic are in scope.
    pub fn lookup(&self, key: ThreadKey) -> Option<&ChatEntry> {
        let entry = self.entries.get(&key.chat)?;
        match &entry.topics {
            Some(topics) => topics
                .contains(&key.topic.unwrap_or(GENERAL_TOPIC))
                .then_some(entry),
            None => Some(entry),
        }
    }

    /// The entry for a chat, ignoring topics.
    pub fn chat(&self, chat: ChatId) -> Option<&ChatEntry> {
        self.entries.get(&chat)
    }

    /// Every entry, sorted by label.
    pub fn entries(&self) -> Vec<&ChatEntry> {
        let mut all: Vec<_> = self.entries.values().collect();
        all.sort_by(|a, b| a.label.cmp(&b.label));
        all
    }
}

fn validate(raw: RawEntry, policy: &Policy) -> Result<ChatEntry> {
    let id = raw.id;
    let profile = raw.profile.unwrap_or_else(|| {
        match raw.kind {
            EntryKind::Dm => DM_PROFILE,
            EntryKind::Group | EntryKind::Forum => GROUP_PROFILE,
        }
        .to_owned()
    });
    if !policy.profiles.contains_key(&profile) {
        return Err(invalid(id, format!("unknown profile {profile:?}")));
    }
    if raw.triggers.is_empty() {
        return Err(invalid(id, "no triggers; remove the entry instead"));
    }
    let mut seen = HashSet::new();
    for trigger in &raw.triggers {
        if !seen.insert(*trigger) {
            return Err(invalid(id, format!("trigger {trigger} listed twice")));
        }
        match (trigger, raw.kind) {
            (TriggerKind::Manual, _) => {
                return Err(invalid(id, "manual is not a scope trigger"));
            }
            (TriggerKind::DmAll, EntryKind::Group | EntryKind::Forum) => {
                return Err(invalid(id, "dm_all only applies to kind = \"dm\""));
            }
            _ => {}
        }
    }
    if raw.triggers.contains(&TriggerKind::Keyword) && raw.keywords.is_empty() {
        return Err(invalid(id, "keyword trigger without keywords"));
    }
    if raw.topics.is_some() && raw.kind != EntryKind::Forum {
        return Err(invalid(id, "topics only apply to kind = \"forum\""));
    }
    let mute_until_unix = raw.mute_until.as_deref().map(parse_date).transpose()?;
    Ok(ChatEntry {
        id: ChatId(id),
        label: raw.label,
        kind: raw.kind,
        topics: raw.topics.map(|t| t.into_iter().map(MessageId).collect()),
        triggers: raw.triggers,
        keywords: raw
            .keywords
            .iter()
            .map(|k| normalize(k.trim()))
            .filter(|k| !k.is_empty())
            .collect(),
        profile,
        mute_until_unix,
    })
}

fn invalid(chat: i64, reason: impl Into<String>) -> Error {
    Error::Invalid {
        file: SCOPE_FILE,
        chat,
        reason: reason.into(),
    }
}

/// Burst limits for one kind of source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// A burst closes after this long with no new message.
    pub debounce_secs: i64,
    /// A burst closes this long after it opened, however active.
    pub burst_max_secs: i64,
    /// A burst closes at this many logical messages; an album counts once.
    pub burst_max_messages: usize,
}

/// Every tunable number of the pipeline, from `policy.toml` over built-in defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Policy {
    /// Burst limits by profile name.
    #[serde(rename = "profile")]
    pub profiles: BTreeMap<String, Profile>,
    /// How long after an anchor the resolver waits before labelling the ticket.
    pub resolve_window_secs: i64,
    /// Anchors older than this when their burst closes go to the digest.
    pub stale_after_secs: i64,
    /// Message text is deleted after this many days.
    pub message_retention_days: i64,
    /// Tickets and metric events are deleted after this many days.
    pub metric_retention_days: i64,
    /// Forwards to Saved Messages up to this long before a `cap:` note belong to it.
    pub manual_before_secs: i64,
    /// Forwards up to this long after a `cap:` note belong to it.
    pub manual_after_secs: i64,
    /// Whether out-of-scope chats are counted, without text, for scope suggestions.
    pub count_out_of_scope: bool,
}

impl Default for Policy {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            DM_PROFILE.to_owned(),
            Profile {
                debounce_secs: DM_DEBOUNCE_SECS,
                burst_max_secs: DM_BURST_MAX_SECS,
                burst_max_messages: DM_BURST_MAX_MESSAGES,
            },
        );
        profiles.insert(
            GROUP_PROFILE.to_owned(),
            Profile {
                debounce_secs: GROUP_DEBOUNCE_SECS,
                burst_max_secs: GROUP_BURST_MAX_SECS,
                burst_max_messages: GROUP_BURST_MAX_MESSAGES,
            },
        );
        Self {
            profiles,
            resolve_window_secs: RESOLVE_WINDOW_SECS,
            stale_after_secs: STALE_AFTER_SECS,
            message_retention_days: MESSAGE_RETENTION_DAYS,
            metric_retention_days: METRIC_RETENTION_DAYS,
            manual_before_secs: MANUAL_BEFORE_SECS,
            manual_after_secs: MANUAL_AFTER_SECS,
            count_out_of_scope: true,
        }
    }
}

impl Policy {
    /// Parses `policy.toml`. Keys it leaves out keep their defaults; profiles it lists replace
    /// or add to the built-in `dm` and `group`.
    ///
    /// # Errors
    ///
    /// [`Error::Parse`] for invalid TOML or unknown keys, [`Error::Invalid`] for a limit that
    /// is zero or negative.
    pub fn parse(text: &str) -> Result<Self> {
        let parsed: Self = toml::from_str(text).map_err(|e| Error::Parse {
            file: POLICY_FILE,
            reason: e.message().to_owned(),
        })?;
        let mut policy = Self::default();
        policy.profiles.extend(parsed.profiles);
        let Self { profiles, .. } = policy;
        let policy = Self { profiles, ..parsed };
        policy.check()?;
        Ok(policy)
    }

    /// The profile named `name`, or the group profile when it no longer exists.
    pub fn profile(&self, name: &str) -> Profile {
        self.profiles
            .get(name)
            .or_else(|| self.profiles.get(GROUP_PROFILE))
            .copied()
            .unwrap_or(Profile {
                debounce_secs: GROUP_DEBOUNCE_SECS,
                burst_max_secs: GROUP_BURST_MAX_SECS,
                burst_max_messages: GROUP_BURST_MAX_MESSAGES,
            })
    }

    fn check(&self) -> Result<()> {
        let positive = [
            ("resolve_window_secs", self.resolve_window_secs),
            ("stale_after_secs", self.stale_after_secs),
            ("message_retention_days", self.message_retention_days),
            ("metric_retention_days", self.metric_retention_days),
            ("manual_before_secs", self.manual_before_secs),
            ("manual_after_secs", self.manual_after_secs),
        ];
        let profiles = self.profiles.iter().flat_map(|(name, p)| {
            [
                (name.as_str(), p.debounce_secs),
                (name.as_str(), p.burst_max_secs),
                (
                    name.as_str(),
                    i64::try_from(p.burst_max_messages).unwrap_or(i64::MAX),
                ),
            ]
        });
        match positive.into_iter().chain(profiles).find(|(_, v)| *v <= 0) {
            Some((what, _)) => Err(Error::Invalid {
                file: POLICY_FILE,
                chat: FILE_LEVEL,
                reason: format!("{what} must be positive"),
            }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) fn test_entry(
    kind: EntryKind,
    triggers: &[TriggerKind],
    keywords: &[&str],
) -> ChatEntry {
    ChatEntry {
        id: ChatId(-100),
        label: "test".into(),
        kind,
        topics: None,
        triggers: triggers.to_vec(),
        keywords: keywords.iter().map(|k| normalize(k)).collect(),
        profile: GROUP_PROFILE.into(),
        mute_until_unix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[chat]]
id = 123456789
label = "Anh Minh"
kind = "dm"
triggers = ["dm_all"]

[[chat]]
id = -1001234567890
label = "Ops HCM"
kind = "forum"
topics = [1, 4521]
triggers = ["mention", "reply_to_me", "keyword"]
keywords = ["Deploy", "hoàn tiền"]
profile = "group"
mute_until = "2026-10-01"
"#;

    fn key(chat: i64, topic: Option<i32>) -> ThreadKey {
        ThreadKey {
            chat: ChatId(chat),
            topic: topic.map(MessageId),
        }
    }

    #[test]
    fn parses_the_documented_example() {
        let scope = Scope::parse(SAMPLE, &Policy::default()).unwrap();
        let dm = scope.lookup(key(123_456_789, None)).unwrap();
        assert_eq!(dm.profile, DM_PROFILE);
        let ops = scope.lookup(key(-1_001_234_567_890, Some(4521))).unwrap();
        assert_eq!(ops.keywords, ["deploy", "hoan tien"]);
        assert!(ops.muted(parse_date("2026-09-30").unwrap()));
        assert!(!ops.muted(parse_date("2026-10-01").unwrap()));
    }

    #[test]
    fn general_topic_is_topic_one_and_other_topics_are_out() {
        let scope = Scope::parse(SAMPLE, &Policy::default()).unwrap();
        assert!(scope.lookup(key(-1_001_234_567_890, None)).is_some());
        assert!(scope.lookup(key(-1_001_234_567_890, Some(99))).is_none());
        assert!(scope.lookup(key(42, None)).is_none());
    }

    #[test]
    fn rejects_rules_that_cannot_work() {
        let cases = [
            ("kind = \"group\"\ntriggers = [\"dm_all\"]", "dm_all"),
            (
                "kind = \"group\"\ntriggers = [\"keyword\"]",
                "without keywords",
            ),
            (
                "kind = \"dm\"\ntriggers = [\"dm_all\"]\nprofile = \"vip\"",
                "unknown profile",
            ),
            (
                "kind = \"group\"\ntopics = [1]\ntriggers = [\"mention\"]",
                "forum",
            ),
            ("kind = \"group\"\ntriggers = [\"manual\"]", "manual"),
            (
                "kind = \"group\"\ntriggers = [\"mention\", \"mention\"]",
                "twice",
            ),
        ];
        for (body, expected) in cases {
            let text = format!("[[chat]]\nid = 1\nlabel = \"x\"\n{body}\n");
            let err = Scope::parse(&text, &Policy::default()).unwrap_err();
            assert!(err.to_string().contains(expected), "{body}: {err}");
        }
    }

    #[test]
    fn rejects_duplicates_and_unknown_keys() {
        let one = "[[chat]]\nid = 1\nlabel = \"x\"\nkind = \"dm\"\ntriggers = [\"dm_all\"]\n";
        let err = Scope::parse(&format!("{one}{one}"), &Policy::default()).unwrap_err();
        assert!(err.to_string().contains("twice"));
        let typo = format!("{one}triger = 1\n");
        assert!(matches!(
            Scope::parse(&typo, &Policy::default()),
            Err(Error::Parse { .. })
        ));
    }

    #[test]
    fn policy_overrides_keep_defaults_for_the_rest() {
        let policy = Policy::parse(
            "stale_after_secs = 60\n[profile.dm]\ndebounce_secs = 30\nburst_max_secs = 200\nburst_max_messages = 5\n",
        )
        .unwrap();
        assert_eq!(policy.stale_after_secs, 60);
        assert_eq!(policy.resolve_window_secs, RESOLVE_WINDOW_SECS);
        assert_eq!(policy.profile(DM_PROFILE).debounce_secs, 30);
        assert_eq!(
            policy.profile(GROUP_PROFILE).debounce_secs,
            GROUP_DEBOUNCE_SECS
        );
        assert!(Policy::parse("stale_after_secs = 0").is_err());
    }
}
