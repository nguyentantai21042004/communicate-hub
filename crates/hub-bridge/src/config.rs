//! Options for [`crate::Bridge`] that are not protocol limits.

use hub_core::TopicId;

use crate::limits::{BACKFILL_RENAME_CAP, BACKFILL_RENAME_GAP_SECS};

/// Static options for one bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// Topic that receives session changes and friend requests.
    ///
    /// `None` keeps those events out of the forum. They are still logged.
    pub ops_topic: Option<TopicId>,
    /// Pin one avatar photo the first time a topic is created.
    ///
    /// `false` skips that photo. The default is `true`.
    pub avatar_card: bool,
    /// Renames one directory pass will enqueue. Further placeholder titles wait for the next pass.
    pub backfill_rename_cap: usize,
    /// Seconds to wait between those renames. The wait is dropped when the pass ends.
    pub backfill_rename_gap_secs: i64,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            ops_topic: None,
            avatar_card: true,
            backfill_rename_cap: BACKFILL_RENAME_CAP,
            backfill_rename_gap_secs: BACKFILL_RENAME_GAP_SECS,
        }
    }
}
