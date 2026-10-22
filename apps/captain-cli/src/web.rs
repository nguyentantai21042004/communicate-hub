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
const FORMAT_JS: &str = include_str!("../web/format.mjs");
const OVERVIEW_JS: &str = include_str!("../web/overview.js");
const CONVERSATIONS_JS: &str = include_str!("../web/conversations.js");
const ANALYTICS_JS: &str = include_str!("../web/analytics.js");
const TELEGRAM_JS: &str = include_str!("../web/telegram.js");
const ZALO_JS: &str = include_str!("../web/zalo.js");
const HEALTH_JS: &str = include_str!("../web/health.js");
const BOOT_JS: &str = include_str!("../web/boot.js");

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

fn router(state: Web) -> Router {
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
        .merge(hub_web::routes(crate::dashboard::Adapter::new(Arc::clone(
            &state.app,
        ))))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(|| asset(APP_JS, "text/javascript")))
        .route("/assets/app.css", get(|| asset(APP_CSS, "text/css")))
        .route(
            "/assets/format.mjs",
            get(|| asset(FORMAT_JS, "text/javascript")),
        )
        .route(
            "/assets/overview.js",
            get(|| asset(OVERVIEW_JS, "text/javascript")),
        )
        .route(
            "/assets/conversations.js",
            get(|| asset(CONVERSATIONS_JS, "text/javascript")),
        )
        .route(
            "/assets/analytics.js",
            get(|| asset(ANALYTICS_JS, "text/javascript")),
        )
        .route(
            "/assets/telegram.js",
            get(|| asset(TELEGRAM_JS, "text/javascript")),
        )
        .route("/assets/zalo.js", get(|| asset(ZALO_JS, "text/javascript")))
        .route(
            "/assets/health.js",
            get(|| asset(HEALTH_JS, "text/javascript")),
        )
        .route("/assets/boot.js", get(|| asset(BOOT_JS, "text/javascript")))
        .nest("/api", api)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
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
    tracing::info!(port, "web UI listening on the loopback interface");
    if let Err(e) = axum::serve(listener, router(state)).await {
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
        return (StatusCode::FORBIDDEN, "unknown host").into_response();
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
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};

    use axum::body::Body;
    use captain_core::{ChatId, ChatKind, ChatSummary, FormattedText, MessageId, MessageView};
    use captain_inbox::{EntryKind, Policy, Scope, ThreadKey, TicketState, TriggerKind};
    use captain_store::{NewTicket, Store};
    use tower::ServiceExt;

    use super::*;
    use crate::app::{App, Config, Db, now_unix};
    use crate::paths::Paths;

    const TOKEN: &str = "test-token";
    const HOST: &str = "127.0.0.1:8787";
    const SCOPE_TOML: &str = "\
[[chat]]
id = 42
label = \"Ada\"
kind = \"dm\"
triggers = [\"dm_all\"]
";

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

    struct Reply {
        status: StatusCode,
        body: String,
        csp: bool,
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "captain-web-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_app(dir: &std::path::Path, scope: Scope) -> Arc<App> {
        let paths = Paths::new(dir);
        let store = Store::open(&paths.store()).unwrap();
        Arc::new(App {
            db: Db::new(store),
            paths,
            profile: "test".into(),
            config: RwLock::new(Config {
                scope: Arc::new(scope),
                policy: Arc::new(Policy::default()),
                scope_missing: false,
            }),
            paused: AtomicBool::new(false),
            started_at: 0,
            live: None,
            ui_url: RwLock::new(None),
        })
    }

    fn web_state(app: Arc<App>) -> Web {
        Web {
            app,
            token: TOKEN.into(),
            hosts: vec![HOST.to_owned(), "localhost:8787".to_owned()].into(),
        }
    }

    async fn call(
        app: &Arc<App>,
        method: &str,
        path: &str,
        host: &str,
        cookie: bool,
        captain: bool,
        body: Option<&str>,
    ) -> Reply {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host);
        if cookie {
            builder = builder.header(header::COOKIE, format!("{COOKIE_NAME}={TOKEN}"));
        }
        if captain {
            builder = builder.header(CSRF_HEADER, "1");
        }
        let request = builder
            .body(Body::from(body.unwrap_or("").to_owned()))
            .expect("request");
        let response = router(web_state(Arc::clone(app)))
            .oneshot(request)
            .await
            .expect("response");
        let status = response.status();
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .is_some();
        let bytes = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .expect("body");
        Reply {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            csp,
        }
    }

    #[tokio::test]
    async fn new_routes_keep_the_existing_guards() {
        let tmp = TempDir::new("guards");
        let app = test_app(&tmp.0, Scope::default());

        let no_cookie = call(&app, "GET", "/api/overview", HOST, false, false, None).await;
        assert_eq!(no_cookie.status, StatusCode::UNAUTHORIZED);
        assert!(no_cookie.body.contains("error"));
        assert!(no_cookie.csp);

        let status = call(&app, "GET", "/api/status", HOST, false, false, None).await;
        assert_eq!(status.status, StatusCode::UNAUTHORIZED);

        let signed_in = call(&app, "GET", "/api/status", HOST, true, false, None).await;
        assert_eq!(signed_in.status, StatusCode::OK);
        assert!(signed_in.body.contains("connection"));

        let bad_host = call(
            &app,
            "GET",
            "/api/overview",
            "evil.example",
            true,
            false,
            None,
        )
        .await;
        assert_eq!(bad_host.status, StatusCode::FORBIDDEN);
        assert_eq!(bad_host.body, "unknown host");

        let post = call(
            &app,
            "POST",
            "/api/history",
            HOST,
            true,
            false,
            Some(r#"{"chat":1,"regex":"a"}"#),
        )
        .await;
        assert_eq!(post.status, StatusCode::FORBIDDEN);
        assert_eq!(post.body, "missing request header");

        let relogin = call(
            &app,
            "POST",
            "/api/zalo/relogin",
            HOST,
            false,
            false,
            Some(""),
        )
        .await;
        assert_eq!(relogin.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn seeded_store_feeds_the_dashboard_without_message_text() {
        let tmp = TempDir::new("seeded");
        let now = now_unix();
        let stamp = now.saturating_sub(60);
        let app = test_app(
            &tmp.0,
            Scope::parse(SCOPE_TOML, &Policy::default()).unwrap(),
        );
        app.db
            .run(move |store| {
                let message = sample_message(stamp);
                if let Some(sender) = &message.sender {
                    store.upsert_chat(sender, None, stamp)?;
                }
                store.put_message(&message, stamp, true)?;
                store.create_ticket(&NewTicket {
                    key: ThreadKey {
                        chat: ChatId(42),
                        topic: None,
                    },
                    chat_label: "Ada".into(),
                    chat_kind: Some(EntryKind::Dm),
                    profile: "dm".into(),
                    trigger: TriggerKind::Mention,
                    trigger_reason: "mentioned in message 1".into(),
                    route: None,
                    state: TicketState::Queued,
                    state_reason: None,
                    stale: false,
                    note: None,
                    opened_at: stamp,
                    closed_at: stamp,
                    anchor: MessageId(1),
                    anchor_date: stamp,
                    messages: vec![MessageId(1)],
                    senders: vec![ChatId(7)],
                })?;
                Ok(())
            })
            .await
            .unwrap();

        let overview = call(
            &app,
            "GET",
            "/api/overview?days=14",
            HOST,
            true,
            false,
            None,
        )
        .await;
        assert_eq!(overview.status, StatusCode::OK);
        assert!(overview.csp);
        assert!(!overview.body.contains("secret phrase"));
        let overview: serde_json::Value = serde_json::from_str(&overview.body).unwrap();
        assert!(overview["kpi"]["needs_attention"].as_u64().unwrap() >= 1);
        assert_eq!(overview["kpi"]["conversations"][0]["watched"], 0);
        let series: u64 = overview["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["messages"].as_u64().unwrap_or(0))
            .sum();
        assert_eq!(series, 0);
        assert_eq!(overview["needs_attention"][0]["title"], "Ada");
        assert_eq!(overview["needs_attention"][0]["trigger"], "mention");
        assert_eq!(overview["modules"].as_array().unwrap().len(), 3);

        let conversations = call(
            &app,
            "GET",
            "/api/conversations?source=telegram",
            HOST,
            true,
            false,
            None,
        )
        .await;
        assert_eq!(conversations.status, StatusCode::OK);
        assert!(!conversations.body.contains("secret phrase"));
        let conversations: serde_json::Value = serde_json::from_str(&conversations.body).unwrap();
        assert_eq!(conversations["total"], 0);

        let zalo_only = call(
            &app,
            "GET",
            "/api/conversations?source=zalo",
            HOST,
            true,
            false,
            None,
        )
        .await;
        let zalo_only: serde_json::Value = serde_json::from_str(&zalo_only.body).unwrap();
        assert_eq!(zalo_only["total"], 0);

        let analytics = call(
            &app,
            "GET",
            "/api/analytics?days=2",
            HOST,
            true,
            false,
            None,
        )
        .await;
        let analytics: serde_json::Value = serde_json::from_str(&analytics.body).unwrap();
        assert_eq!(analytics["heatmap"].as_array().unwrap().len(), 168);
        assert_eq!(analytics["funnel"]["draft"], "not available in P1");
        assert_eq!(analytics["response_p50"].as_array().unwrap().len(), 2);

        let telegram = call(
            &app,
            "GET",
            "/api/telegram?days=14",
            HOST,
            true,
            false,
            None,
        )
        .await;
        let telegram: serde_json::Value = serde_json::from_str(&telegram.body).unwrap();
        assert_eq!(telegram["triggers"].as_array().unwrap().len(), 4);
        assert_eq!(telegram["triggers"][0]["trigger"], "dm_all");
        assert!(telegram["scope"]["watched"].as_u64().unwrap() >= 1);
        assert!(telegram["scope"]["with_messages"].as_u64().unwrap() >= 1);
        assert_eq!(telegram["top_senders"][0]["name"], "chat 7");
        assert_eq!(telegram["top_senders"][0]["count"], 1);
        assert!(
            !telegram["top_senders"]
                .to_string()
                .contains("secret phrase")
        );
        assert_eq!(telegram["session"]["storage"], "keychain");
        assert_eq!(telegram["session"]["connection"], "offline");

        let zalo = call(&app, "GET", "/api/zalo", HOST, true, false, None).await;
        assert_eq!(zalo.status, StatusCode::OK);
        let zalo: serde_json::Value = serde_json::from_str(&zalo.body).unwrap();
        assert_eq!(zalo["bridge"]["state"], "down");
        assert_eq!(zalo["send_queue"]["waiting"], 0);

        let modules = call(&app, "GET", "/api/modules", HOST, true, false, None).await;
        let healthz = call(&app, "GET", "/api/healthz", HOST, true, false, None).await;
        assert_eq!(modules.body, healthz.body);
        assert!(modules.body.contains("captain-daemon"));

        let events = call(&app, "GET", "/api/events", HOST, true, false, None).await;
        let events: serde_json::Value = serde_json::from_str(&events.body).unwrap();
        assert!(events["events"].as_array().unwrap().is_empty());

        let history = call(
            &app,
            "POST",
            "/api/history",
            HOST,
            true,
            true,
            Some(r#"{"chat":42,"regex":"secret"}"#),
        )
        .await;
        assert_eq!(history.status, StatusCode::BAD_REQUEST);
        assert!(history.body.contains("daemon is not running"));
        assert!(!history.body.contains("secret phrase"));

        let relogin = call(&app, "POST", "/api/zalo/relogin", HOST, true, true, None).await;
        assert_eq!(relogin.status, StatusCode::NOT_IMPLEMENTED);
        assert!(relogin.body.contains("zalo QR login is not wired"));

        let retry = call(&app, "POST", "/api/zalo/retry/4", HOST, true, true, None).await;
        assert_eq!(retry.status, StatusCode::NOT_IMPLEMENTED);
        assert!(retry.body.contains("zalo retry is not wired"));

        let bad = call(&app, "GET", "/api/overview?days=0", HOST, true, false, None).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(bad.body.contains("days must be at least 1"));
    }

    #[tokio::test]
    async fn empty_store_returns_zeros_and_bridge_down() {
        let tmp = TempDir::new("empty");
        let app = test_app(&tmp.0, Scope::default());
        let overview = call(
            &app,
            "GET",
            "/api/overview?days=2&source=telegram",
            HOST,
            true,
            false,
            None,
        )
        .await;
        assert_eq!(overview.status, StatusCode::OK);
        let overview: serde_json::Value = serde_json::from_str(&overview.body).unwrap();
        assert_eq!(overview["kpi"]["needs_attention"], 0);
        assert_eq!(overview["series"].as_array().unwrap().len(), 2);
        assert_eq!(overview["modules"][0]["state"], "down");

        let conversations = call(&app, "GET", "/api/conversations", HOST, true, false, None).await;
        let conversations: serde_json::Value = serde_json::from_str(&conversations.body).unwrap();
        assert_eq!(conversations["total"], 0);

        let bad_source = call(
            &app,
            "GET",
            "/api/conversations?source=nope",
            HOST,
            true,
            false,
            None,
        )
        .await;
        assert_eq!(bad_source.status, StatusCode::BAD_REQUEST);
    }

    fn sample_message(stamp: i64) -> MessageView {
        let summary = |id, kind| ChatSummary {
            id: ChatId(id),
            kind,
            name: Some(format!("chat {id}")),
            username: None,
            is_bot: false,
        };
        MessageView {
            chat: summary(42, ChatKind::User),
            id: MessageId(1),
            sender: Some(summary(7, ChatKind::User)),
            outgoing: false,
            date_unix: stamp,
            text: FormattedText::plain("secret phrase"),
            media: None,
            reply_to: None,
            album_id: None,
            mentioned: true,
            topic: None,
            quote_text: None,
            forward: None,
            edited_at_unix: None,
        }
    }
}
