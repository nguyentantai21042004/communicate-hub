//! Options for [`crate::Bridge`] that are not protocol limits.

use hub_core::TopicId;

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
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            ops_topic: None,
            avatar_card: true,
        }
    }
}
