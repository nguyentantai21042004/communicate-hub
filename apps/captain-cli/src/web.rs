//! The monitoring UI: a small HTTP server on the loopback interface.
//!
//! SECURITY: anything that can reach the port could read tickets, so the server binds to
//! 127.0.0.1 only, rejects requests whose `Host` is not that address (DNS rebinding), needs
//! the token from `captain ui` as a cookie, and needs a custom header on every POST, which a
//! cross-site form cannot set. Message text only leaves the server when a ticket is opened
//! with `text=1`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::app::App;

/// Port the UI listens on unless `CAPTAIN_UI_PORT` says otherwise. It is never moved to a
/// free port: a second server on another port is how duplicate daemons go unnoticed.
pub const DEFAULT_PORT: u16 = 8787;
/// Overrides [`DEFAULT_PORT`].
pub const PORT_ENV: &str = "CAPTAIN_UI_PORT";

const COOKIE_NAME: &str = "captain_ui";
const CSRF_HEADER: &str = "x-captain";
const TOKEN_QUERY: &str = "token";
const SECS_PER_DAY: i64 = 86_400;
const DEFAULT_REPORT_DAYS: i64 = 14;
const MAX_REPORT_DAYS: i64 = 180;
const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

const INDEX_HTML: &str = include_str!("../web/index.html");
const LOCKED_HTML: &str = include_str!("../web/locked.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");

/// Where the UI listens.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub port: u16,
}

impl Settings {
    /// Reads `CAPTAIN_UI_PORT`, falling back to [`DEFAULT_PORT`].
    pub fn from_env() -> Result<Self> {
        let port = match std::env::var(PORT_ENV) {
            Ok(v) => v
                .trim()
                .parse()
                .with_context(|| format!("{PORT_ENV} must be a port number"))?,
            Err(_) => DEFAULT_PORT,
        };
        Ok(Self { port })
    }

    pub fn base_url(self) -> String {
        format!("http://{}:{}/", Ipv4Addr::LOCALHOST, self.port)
    }

    /// The URL that signs a browser in; `index` trades the query for a cookie.
    pub fn login_url(self, token: &str) -> String {
        format!("{}?{TOKEN_QUERY}={token}", self.base_url())
    }
}

/// Binds the loopback port, failing when it is taken instead of picking another.
pub async fn bind(app: &App, settings: &Settings) -> Result<TcpListener> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, settings.port));
    let listener = TcpListener::bind(addr).await.with_context(|| {
        format!(
            "port {} is taken; stop whatever holds it, or set {PORT_ENV}",
            settings.port
        )
    })?;
    if let Ok(mut url) = app.ui_url.write() {
        *url = Some(settings.base_url());
    }
    Ok(listener)
}

#[derive(Clone)]
struct Web {
    app: Arc<App>,
    token: Arc<str>,
    hosts: Arc<[String]>,
}

/// Serves until the task is cancelled.
pub async fn serve(app: Arc<App>, listener: TcpListener, token: String) {
    let port = listener.local_addr().map_or(DEFAULT_PORT, |a| a.port());
    let state = Web {
        app,
        token: token.into(),
        hosts: vec![
            format!("{}:{port}", Ipv4Addr::LOCALHOST),
            format!("localhost:{port}"),
        ]
        .into(),
    };
    let api = Router::new()
        .route("/status", get(status))
        .route("/tickets", get(tickets))
        .route("/tickets/{id}", get(ticket))
        .route("/tickets/{id}/label", post(label))
        .route("/report", get(report))
        .route("/scope", get(scope))
        .route("/scope/reload", post(reload))
        .route("/pause", post(pause))
        .route("/resume", post(resume))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    let router = Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(|| asset(APP_JS, "text/javascript")))
        .route("/assets/app.css", get(|| asset(APP_CSS, "text/css")))
        .nest("/api", api)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    tracing::info!(port, "web UI listening on the loopback interface");
    if let Err(e) = axum::serve(listener, router).await {
        tracing::error!(error = %e, "web UI stopped");
    }
}

/// Host check, POST header check and security headers, for every route.
async fn guard(State(web): State<Web>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok());
    if !host.is_some_and(|h| web.hosts.iter().any(|allowed| allowed == h)) {
        return (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response();
    }
    if request.method() != Method::GET && !request.headers().contains_key(CSRF_HEADER) {
        return (StatusCode::FORBIDDEN, "missing request header").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn require_token(State(web): State<Web>, request: Request, next: Next) -> Response {
    if has_token(request.headers(), &web.token) {
        next.run(request).await
    } else {
        api_error(StatusCode::UNAUTHORIZED, "open the UI with `captain ui`")
    }
}

fn has_token(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == COOKIE_NAME && same(value.as_bytes(), token.as_bytes()))
}

/// Compares without an early exit, so response time says nothing about the token.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Deserialize)]
struct IndexQuery {
    token: Option<String>,
}

/// With `?token=` the token moves into a cookie and the URL is cleaned; without a valid
/// cookie the page explains how to get in.
async fn index(
    State(web): State<Web>,
    headers: HeaderMap,
    Query(query): Query<IndexQuery>,
) -> Response {
    if let Some(token) = query.token {
        if !same(token.as_bytes(), web.token.as_bytes()) {
            return (StatusCode::UNAUTHORIZED, html(LOCKED_HTML)).into_response();
        }
        let cookie = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict");
        let mut response = Redirect::to("/").into_response();
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
        return response;
    }
    if has_token(&headers, &web.token) {
        html(INDEX_HTML).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, html(LOCKED_HTML)).into_response()
    }
}

fn html(body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body)
}

async fn asset(body: &'static str, mime: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, mime)], body)
}

fn api_error(status: StatusCode, message: impl std::fmt::Display) -> Response {
    (status, axum::Json(json!({ "error": message.to_string() }))).into_response()
}

fn reply(result: Result<Value>) -> Response {
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(e) => api_error(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn status(State(web): State<Web>) -> Response {
    reply(web.app.status().await)
}

#[derive(Deserialize)]
struct TicketsQuery {
    state: Option<String>,
    trigger: Option<String>,
    days: Option<i64>,
    #[serde(default)]
    unlabelled: bool,
    limit: Option<usize>,
}

async fn tickets(State(web): State<Web>, Query(q): Query<TicketsQuery>) -> Response {
    let state = q.state.filter(|s| !s.is_empty());
    let trigger = q.trigger.filter(|s| !s.is_empty());
    let within = q.days.map(|d| d.clamp(1, MAX_REPORT_DAYS) * SECS_PER_DAY);
    reply(
        web.app
            .tickets(
                state.as_deref(),
                trigger.as_deref(),
                within,
                q.unlabelled,
                q.limit,
            )
            .await,
    )
}

#[derive(Deserialize)]
struct TicketQuery {
    #[serde(default)]
    text: u8,
}

async fn ticket(
    State(web): State<Web>,
    Path(id): Path<i64>,
    Query(q): Query<TicketQuery>,
) -> Response {
    reply(web.app.ticket(id, q.text == 1).await)
}

#[derive(Deserialize)]
struct LabelBody {
    label: Option<String>,
}

async fn label(
    State(web): State<Web>,
    Path(id): Path<i64>,
    axum::Json(body): axum::Json<LabelBody>,
) -> Response {
    reply(web.app.label(id, body.label.as_deref()).await)
}

#[derive(Deserialize)]
struct ReportQuery {
    days: Option<i64>,
}

async fn report(State(web): State<Web>, Query(q): Query<ReportQuery>) -> Response {
    let days = q
        .days
        .unwrap_or(DEFAULT_REPORT_DAYS)
        .clamp(1, MAX_REPORT_DAYS);
    reply(web.app.report(days * SECS_PER_DAY).await)
}

async fn scope(State(web): State<Web>) -> Response {
    reply(Ok(web.app.scope_view()))
}

async fn reload(State(web): State<Web>) -> Response {
    reply(web.app.reload())
}

async fn pause(State(web): State<Web>) -> Response {
    reply(web.app.set_paused(true).await)
}

async fn resume(State(web): State<Web>) -> Response {
    reply(web.app.set_paused(false).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_must_match_the_cookie_exactly() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; captain_ui=abc"),
        );
        assert!(has_token(&headers, "abc"));
        assert!(!has_token(&headers, "abcd"));
        assert!(!has_token(&HeaderMap::new(), "abc"));
        assert!(same(b"x", b"x"));
        assert!(!same(b"x", b"y"));
    }
}
