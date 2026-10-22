//! Options for [`crate::Bridge`] that are not protocol limits.

use hub_core::TopicId;

/// Static options for one bridge.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeConfig {
    /// Topic that receives session changes and friend requests.
    ///
    /// `None` keeps those events out of the forum. They are still logged.
    pub ops_topic: Option<TopicId>,
}
