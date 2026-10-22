//! Axum routes for the dashboard. They carry no access checks of their own.
//!
//! Mount the router under the existing server so the loopback bind, `Host` check, token
//! cookie, `X-Captain` header, and content security policy cover these paths too.

use axum::Json;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};

use crate::api::HubApi;
use crate::error::Error;
use crate::limits::MAX_HISTORY_BODY;
use crate::query;

/// Routes for the dashboard API, without the `/api` prefix.
///
/// `S` is unused at runtime. [`Router::with_state`](axum::Router::with_state) erases `api` so
/// this router can merge into a parent that has a different state and therefore a different
/// middleware stack.
pub fn routes<A, S>(api: A) -> Router<S>
where
    A: HubApi + Clone + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/overview", get(overview::<A>))
        .route("/conversations", get(conversations::<A>))
        .route("/analytics", get(analytics::<A>))
        .route("/telegram", get(telegram::<A>))
        .route("/history", post(history::<A>))
        .route("/zalo", get(zalo::<A>))
        .route("/zalo/relogin", post(zalo_relogin::<A>))
        .route("/zalo/retry/{id}", post(zalo_retry::<A>))
        .route("/modules", get(modules::<A>))
        .route("/healthz", get(modules::<A>))
        .route("/events", get(events::<A>))
        .with_state(api)
}

async fn overview<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::overview(request.uri().query()) {
        Ok(query) => finish(api.overview(query).await),
        Err(error) => error.into_response(),
    }
}

async fn conversations<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::conversations(request.uri().query()) {
        Ok(query) => finish(api.conversations(query).await),
        Err(error) => error.into_response(),
    }
}

async fn analytics<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::analytics(request.uri().query()) {
        Ok(query) => finish(api.analytics(query).await),
        Err(error) => error.into_response(),
    }
}

async fn telegram<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::days(request.uri().query()) {
        Ok(query) => finish(api.telegram(query).await),
        Err(error) => error.into_response(),
    }
}

async fn zalo<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::days(request.uri().query()) {
        Ok(query) => finish(api.zalo(query).await),
        Err(error) => error.into_response(),
    }
}

async fn modules<A>(State(api): State<A>) -> axum::response::Response
where
    A: HubApi + Clone,
{
    finish(api.modules().await)
}

async fn events<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    match query::events_limit(request.uri().query()) {
        Ok(limit) => finish(api.events(limit).await),
        Err(error) => error.into_response(),
    }
}

async fn history<A>(State(api): State<A>, request: Request) -> axum::response::Response
where
    A: HubApi + Clone,
{
    let bytes = match axum::body::to_bytes(request.into_body(), MAX_HISTORY_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Error::BadRequest("history body is too large".into()).into_response();
        }
    };
    let parsed = match query::history_body(&bytes) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let include_text = parsed.text;
    match api.history(parsed).await {
        Ok(mut page) => {
            // SECURITY: the route drops text even if an implementation included it.
            if !include_text {
                for row in &mut page.matches {
                    row.text = None;
                }
            }
            Json(page).into_response()
        }
        Err(error) => error.into_response(),
    }
}

async fn zalo_relogin<A>(State(api): State<A>) -> axum::response::Response
where
    A: HubApi + Clone,
{
    finish(api.zalo_relogin().await)
}

async fn zalo_retry<A>(State(api): State<A>, Path(id): Path<String>) -> axum::response::Response
where
    A: HubApi + Clone,
{
    let Ok(id) = id.parse::<i64>() else {
        return Error::BadRequest("retry id must be an integer".into()).into_response();
    };
    finish(api.zalo_retry(id).await)
}

fn finish<T: serde::Serialize>(result: crate::error::Result<T>) -> axum::response::Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}
