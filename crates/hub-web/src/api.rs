//! Data the routes ask the application for.
//!
//! The application reads the stores and the daemon. This crate does not.

use std::future::Future;

use crate::error::Result;
use crate::model::{
    Analytics, AnalyticsQuery, ConversationPage, ConversationQuery, DaysQuery, EventPage,
    HistoryRequest, HistoryResponse, ModuleList, Overview, OverviewQuery, ReloginAck, RetryAck,
    TelegramPage, ZaloPage,
};

/// Supplies one dashboard response per call.
///
/// List values must not contain message text. Zalo external ids must already be masked.
/// [`HubApi::zalo`] returns a page with `bridge.state` of `down` when the bridge is stopped;
/// it does not fail that call.
pub trait HubApi: Send + Sync {
    /// Overview for `query`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the numbers cannot be read.
    fn overview(&self, query: OverviewQuery) -> impl Future<Output = Result<Overview>> + Send;

    /// One page of conversations.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the list cannot be read.
    fn conversations(
        &self,
        query: ConversationQuery,
    ) -> impl Future<Output = Result<ConversationPage>> + Send;

    /// Analytics for `query`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the numbers cannot be read.
    fn analytics(&self, query: AnalyticsQuery) -> impl Future<Output = Result<Analytics>> + Send;

    /// Telegram page for the window.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the numbers cannot be read.
    fn telegram(&self, query: DaysQuery) -> impl Future<Output = Result<TelegramPage>> + Send;

    /// History search. The route drops `text` again when the request did not ask for it.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the pattern is invalid or the daemon cannot search.
    fn history(&self, body: HistoryRequest)
    -> impl Future<Output = Result<HistoryResponse>> + Send;

    /// Zalo page. A stopped bridge is a successful page, not an error.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the last stored numbers cannot be read.
    fn zalo(&self, query: DaysQuery) -> impl Future<Output = Result<ZaloPage>> + Send;

    /// `captain-daemon`, `zalo-bridge`, and `bridge`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when health cannot be read.
    fn modules(&self) -> impl Future<Output = Result<ModuleList>> + Send;

    /// Module events, newest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Failed`] when the log cannot be read.
    fn events(&self, limit: u32) -> impl Future<Output = Result<EventPage>> + Send;

    /// Starts the Zalo QR login.
    ///
    /// # Errors
    ///
    /// [`crate::Error::NotImplemented`] until a Zalo admin can start the flow, and
    /// [`crate::Error::Failed`] when a wired flow rejects the call.
    fn zalo_relogin(&self) -> impl Future<Output = Result<ReloginAck>> + Send;

    /// Asks the bridge to retry failed event `id`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::NotImplemented`] until a Zalo admin can retry, and
    /// [`crate::Error::Failed`] when a wired retry rejects the id.
    fn zalo_retry(&self, id: i64) -> impl Future<Output = Result<RetryAck>> + Send;
}
