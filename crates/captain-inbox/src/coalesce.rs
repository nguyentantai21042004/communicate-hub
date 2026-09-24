//! Coalescing: consecutive messages on one thread become one burst, and one burst one ticket.

use std::collections::{HashMap, HashSet};

use captain_core::{ChatId, MessageId};

use crate::config::Profile;
use crate::ticket::ThreadKey;
use crate::trigger::Trigger;

/// Messages on one thread that arrived close together, with the triggers they fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Burst {
    /// Thread the burst is on.
    pub key: ThreadKey,
    /// Profile whose limits apply.
    pub profile: String,
    /// When the first triggering message arrived.
    pub opened_at: i64,
    /// When the last message arrived.
    pub last_at: i64,
    /// Every message id, in arrival order.
    pub messages: Vec<MessageId>,
    first_trigger: Trigger,
    anchor: MessageId,
    /// Distinct senders.
    pub senders: Vec<ChatId>,
    limits: Profile,
    albums: HashSet<i64>,
    logical: usize,
}

impl Burst {
    /// The last triggering message, which a reply would answer.
    pub fn anchor(&self) -> MessageId {
        self.anchor
    }

    /// The first trigger, which names the ticket.
    pub fn trigger(&self) -> &Trigger {
        &self.first_trigger
    }

    /// Messages, counting an album once.
    pub fn logical_len(&self) -> usize {
        self.logical
    }

    fn push(&mut self, id: MessageId, album: Option<i64>, sender: Option<ChatId>, now: i64) {
        self.messages.push(id);
        self.last_at = now;
        let new_logical = album.is_none_or(|a| self.albums.insert(a));
        if new_logical {
            self.logical += 1;
        }
        if let Some(sender) = sender
            && !self.senders.contains(&sender)
        {
            self.senders.push(sender);
        }
    }

    fn deadline(&self) -> i64 {
        (self.last_at + self.limits.debounce_secs).min(self.opened_at + self.limits.burst_max_secs)
    }

    fn full(&self) -> bool {
        self.logical >= self.limits.burst_max_messages
    }
}

/// One incoming, in-scope message offered to the [`Coalescer`].
#[derive(Debug, Clone)]
pub struct Arrival {
    /// Thread it is on.
    pub key: ThreadKey,
    /// Its id.
    pub id: MessageId,
    /// Album it belongs to.
    pub album: Option<i64>,
    /// Who sent it.
    pub sender: Option<ChatId>,
    /// What it fired, if anything.
    pub trigger: Option<Trigger>,
}

/// Open bursts, one per thread.
#[derive(Debug, Default)]
pub struct Coalescer {
    open: HashMap<ThreadKey, Burst>,
}

impl Coalescer {
    /// Adds an incoming message at time `now`, using `limits` of `profile` for a new burst.
    ///
    /// A message joins the thread's open burst whether or not it triggers. With no open burst,
    /// only a triggering message opens one. Returns a burst that this message filled up, which
    /// is closed at once. Call [`Self::due`] first so expired bursts do not absorb it.
    pub fn observe(
        &mut self,
        arrival: Arrival,
        profile: &str,
        limits: Profile,
        now: i64,
    ) -> Option<Burst> {
        let Arrival {
            key,
            id,
            album,
            sender,
            trigger,
        } = arrival;
        let burst = match self.open.get_mut(&key) {
            Some(burst) => {
                if trigger.is_some() {
                    burst.anchor = id;
                }
                burst
            }
            None => {
                // Only a triggering message opens a burst.
                let first_trigger = trigger?;
                self.open.entry(key).or_insert_with(|| Burst {
                    key,
                    profile: profile.to_owned(),
                    opened_at: now,
                    last_at: now,
                    messages: Vec::new(),
                    first_trigger,
                    anchor: id,
                    senders: Vec::new(),
                    limits,
                    albums: HashSet::new(),
                    logical: 0,
                })
            }
        };
        burst.push(id, album, sender, now);
        if burst.full() {
            return self.open.remove(&key);
        }
        None
    }

    /// The account wrote on `key`: it is handling the thread itself, so its open burst is
    /// cancelled and returned.
    pub fn outgoing(&mut self, key: ThreadKey) -> Option<Burst> {
        self.open.remove(&key)
    }

    /// Closes and returns every burst whose quiet period or maximum length ran out by `now`.
    pub fn due(&mut self, now: i64) -> Vec<Burst> {
        let keys: Vec<ThreadKey> = self
            .open
            .iter()
            .filter(|(_, b)| b.deadline() <= now)
            .map(|(k, _)| *k)
            .collect();
        let mut closed: Vec<Burst> = keys.iter().filter_map(|k| self.open.remove(k)).collect();
        closed.sort_by_key(|b| b.opened_at);
        closed
    }

    /// When the next burst will close, if any is open.
    pub fn next_deadline(&self) -> Option<i64> {
        self.open.values().map(Burst::deadline).min()
    }

    /// Number of open bursts.
    pub fn open_count(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::TriggerKind;

    const LIMITS: Profile = Profile {
        debounce_secs: 10,
        burst_max_secs: 60,
        burst_max_messages: 3,
    };

    fn key(topic: Option<i32>) -> ThreadKey {
        ThreadKey {
            chat: ChatId(-5),
            topic: topic.map(MessageId),
        }
    }

    fn arrival(topic: Option<i32>, id: i32, album: Option<i64>, fires: bool) -> Arrival {
        Arrival {
            key: key(topic),
            id: MessageId(id),
            album,
            sender: Some(ChatId(7)),
            trigger: fires.then(|| Trigger {
                kind: TriggerKind::Mention,
                reason: "t".into(),
            }),
        }
    }

    #[test]
    fn quiet_messages_do_not_open_a_burst() {
        let mut c = Coalescer::default();
        assert!(
            c.observe(arrival(None, 1, None, false), "group", LIMITS, 0)
                .is_none()
        );
        assert_eq!(c.open_count(), 0);
    }

    #[test]
    fn follow_ups_join_until_the_thread_goes_quiet() {
        let mut c = Coalescer::default();
        c.observe(arrival(None, 1, None, true), "group", LIMITS, 0);
        c.observe(arrival(None, 2, None, false), "group", LIMITS, 8);
        assert!(c.due(17).is_empty());
        assert_eq!(c.next_deadline(), Some(18));
        let closed = c.due(18);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].messages, [MessageId(1), MessageId(2)]);
        assert_eq!(closed[0].anchor(), MessageId(1));
    }

    #[test]
    fn album_counts_once_and_full_bursts_close_immediately() {
        let mut c = Coalescer::default();
        c.observe(arrival(None, 1, None, true), "group", LIMITS, 0);
        c.observe(arrival(None, 2, Some(9), false), "group", LIMITS, 1);
        assert!(
            c.observe(arrival(None, 3, Some(9), false), "group", LIMITS, 1)
                .is_none()
        );
        let full = c
            .observe(arrival(None, 4, None, true), "group", LIMITS, 2)
            .unwrap();
        assert_eq!(full.logical_len(), 3);
        assert_eq!(full.anchor(), MessageId(4));
        assert_eq!(c.open_count(), 0);
    }

    #[test]
    fn topics_are_separate_threads_and_own_messages_cancel() {
        let mut c = Coalescer::default();
        c.observe(arrival(Some(4), 1, None, true), "group", LIMITS, 0);
        c.observe(arrival(Some(5), 2, None, true), "group", LIMITS, 0);
        assert_eq!(c.open_count(), 2);
        let cancelled = c.outgoing(key(Some(4))).unwrap();
        assert_eq!(cancelled.messages, [MessageId(1)]);
        assert_eq!(c.open_count(), 1);
    }

    #[test]
    fn busy_threads_close_at_the_maximum_length() {
        let mut c = Coalescer::default();
        let limits = Profile {
            burst_max_messages: 100,
            ..LIMITS
        };
        for (i, t) in (0..=60).step_by(5).enumerate() {
            let id = i32::try_from(i).unwrap();
            c.observe(arrival(None, id, None, i == 0), "group", limits, t);
            if t < 60 {
                assert!(c.due(t).is_empty());
            }
        }
        assert_eq!(c.due(60).len(), 1);
    }
}
