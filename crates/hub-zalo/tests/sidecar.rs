//! Fixture tests for the Zalo sidecar supervisor.
//!
//! They spawn `tests/fixtures/fake-sidecar.mjs` and never talk to Zalo.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use hub_core::{
    Attachment, AttachmentKind, AttachmentSource, ConversationId, ConversationKind, Network,
    NetworkAdapter, OutgoingMessage,
};
use hub_zalo::{
    CRASH_LOOP_CAP, Error, MEDIA_RETENTION, METHOD_NAMES, RESTART_INITIAL_BACKOFF, RunPhase,
    Sidecar, SidecarConfig, ZaloAdapter,
};
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

#[allow(clippy::print_stderr)] // not inside a #[test] fn, so the test print allow-list misses it
fn node_available() -> bool {
    match std::process::Command::new("node").arg("--version").output() {
        Ok(output) if output.status.success() => true,
        _ => {
            eprintln!("skipping hub-zalo sidecar fixture tests: node is not on PATH");
            false
        }
    }
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-sidecar.mjs")
}

#[allow(clippy::unwrap_used)] // not inside a #[test] fn, so the test unwrap allow-list misses it
fn temp_dir(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hub-zalo-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let creds = dir.join("credentials.json");
    std::fs::write(&creds, b"{}\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600));
    }
    dir
}

fn config(dir: &std::path::Path, mode: &str) -> SidecarConfig {
    SidecarConfig::new(
        "node",
        vec![fixture().into(), OsString::from(mode)],
        dir.join("credentials.json"),
        dir.to_path_buf(),
    )
}

use std::ffi::OsString;

fn conversation() -> ConversationId {
    ConversationId {
        network: Network::Zalo,
        kind: ConversationKind::Dm,
        external_id: "user-1".into(),
    }
}

async fn drive_until(mut ready: impl FnMut() -> bool) {
    for _ in 0..40 {
        if ready() {
            return;
        }
        // The child exits on the real clock. Paused tokio time does not observe
        // SIGCHLD, so this sleep lets the OS report the exit before the next poll.
        std::thread::sleep(Duration::from_millis(30));
        tokio::task::yield_now().await;
    }
    assert!(ready(), "sidecar condition was not met");
}

struct Warns;

static STDOUT_IGNORED: AtomicUsize = AtomicUsize::new(0);

impl Subscriber for Warns {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut message = String::new();
        event.record(&mut Message(&mut message));
        if message.contains("stdout line ignored") {
            STDOUT_IGNORED.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

struct Message<'a>(&'a mut String);

impl Visit for Message<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        }
    }
}

fn install_warns() {
    let _ = tracing::subscriber::set_global_default(Warns);
}

#[test]
fn contract_names_are_stable() {
    assert!(METHOD_NAMES.contains(&"send.text"));
    assert!(METHOD_NAMES.contains(&"directory.group_members"));
    assert_eq!(CRASH_LOOP_CAP, 5);
    assert_eq!(MEDIA_RETENTION, Duration::from_secs(7 * 24 * 60 * 60));
    assert_eq!(RESTART_INITIAL_BACKOFF, Duration::from_secs(1));
}

#[tokio::test(flavor = "current_thread")]
async fn request_response_and_event_normalization() {
    if !node_available() {
        return;
    }
    let dir = temp_dir("rpc");
    let (adapter, admin) = ZaloAdapter::start(config(&dir, "run")).unwrap();
    let health: Value = adapter.sidecar().call("health", json!({})).await.unwrap();
    assert_eq!(health["session"], "valid");

    let sent = adapter
        .send(OutgoingMessage {
            conversation: conversation(),
            text: "ping".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap();
    assert_eq!(sent.message_id, "m-sent");

    let limited = adapter
        .send(OutgoingMessage {
            conversation: conversation(),
            text: "limit".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        limited,
        hub_core::Error::RateLimited {
            retry_after_secs: 7
        }
    ));

    let url_only = adapter
        .send(OutgoingMessage {
            conversation: conversation(),
            text: String::new(),
            attachments: vec![Attachment {
                kind: AttachmentKind::Photo,
                mime: None,
                size_bytes: None,
                source: AttachmentSource::Url("https://example.com/a.jpg".into()),
                file_name: None,
            }],
            reply_to: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(url_only, hub_core::Error::Adapter { .. }));

    let empty = adapter
        .send(OutgoingMessage {
            conversation: conversation(),
            text: "  ".into(),
            attachments: Vec::new(),
            reply_to: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(empty, hub_core::Error::Adapter { detail } if detail == "unsupported"));

    let two = adapter
        .send(OutgoingMessage {
            conversation: conversation(),
            text: "cap".into(),
            attachments: vec![
                Attachment {
                    kind: AttachmentKind::File,
                    mime: None,
                    size_bytes: None,
                    source: AttachmentSource::LocalPath("/tmp/a.bin".into()),
                    file_name: None,
                },
                Attachment {
                    kind: AttachmentKind::Voice,
                    mime: None,
                    size_bytes: None,
                    source: AttachmentSource::LocalPath("/tmp/b.m4a".into()),
                    file_name: None,
                },
            ],
            reply_to: None,
        })
        .await
        .unwrap();
    assert_eq!(two.message_id, "m-file-2");

    adapter
        .sidecar()
        .call(
            "test.emit",
            json!({
                "event": "login.qr.state",
                "data": {"state": "waiting", "png_base64": "aaa"}
            }),
        )
        .await
        .unwrap();
    adapter
        .sidecar()
        .call(
            "test.emit",
            json!({
                "event": "session",
                "data": {"state": "disconnected", "detail": "close_1006"}
            }),
        )
        .await
        .unwrap();
    adapter
        .sidecar()
        .call(
            "test.emit",
            json!({
                "event": "message",
                "data": {
                    "conversation": "zalo:dm:user-1",
                    "message_id": "m-1",
                    "sender_id": "user-1",
                    "sender_display_name": "An",
                    "text": "hello",
                    "attachments": [{"kind": "photo", "local_path": "/tmp/a.jpg", "file_name": "a.jpg"}],
                    "sent_at_unix_secs": 1700000000,
                    "mentions": [],
                    "from_self": true
                }
            }),
        )
        .await
        .unwrap();
    let event = adapter.next_event().await.unwrap().unwrap();
    match event {
        hub_core::RemoteEvent::Message(message) => {
            assert_eq!(message.text, "hello");
            assert_eq!(
                message.attachments[0].source,
                AttachmentSource::LocalPath("/tmp/a.jpg".into())
            );
        }
        other => {
            let shown = format!("{other:?}");
            assert!(shown.is_empty(), "{shown}");
        }
    }
    let qr = admin.next_qr_state().await.unwrap();
    assert_eq!(qr.state, "waiting");
    assert_eq!(qr.png_base64.as_deref(), Some("aaa"));

    adapter
        .sidecar()
        .call(
            "test.emit",
            json!({"event": "listener", "data": {"state": "up", "reason": "started"}}),
        )
        .await
        .unwrap();
    adapter
        .sidecar()
        .call(
            "test.emit",
            json!({"event": "session", "data": {"state": "valid"}}),
        )
        .await
        .unwrap();
    let snapshot = adapter.health().await.unwrap();
    assert_eq!(snapshot.module, "zalo-bridge");
    assert_eq!(snapshot.state, hub_core::ModuleState::Ok);
    assert_eq!(
        snapshot.detail,
        "listener=up session=valid last_restart=0 restarts_24h=0"
    );

    adapter.sidecar().shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_malformed_line_and_stderr() {
    if !node_available() {
        return;
    }
    install_warns();
    let dir = temp_dir("io");
    let mut cfg = config(&dir, "run");
    cfg.request_timeout = Duration::from_millis(100);
    let sidecar = Sidecar::new(cfg).unwrap();
    let _keep = sidecar.subscribe();
    sidecar.launch().unwrap();

    let hung = sidecar.call("test.hang", json!({})).await.unwrap_err();
    assert!(matches!(hung, Error::Timeout));

    let before = STDOUT_IGNORED.load(Ordering::SeqCst);
    let malformed: Value = sidecar.call("test.malformed", json!({})).await.unwrap();
    assert_eq!(malformed, json!({}));
    drive_until(|| STDOUT_IGNORED.load(Ordering::SeqCst) > before).await;

    let stderr: Value = sidecar.call("test.stderr", json!({})).await.unwrap();
    assert_eq!(stderr["from"], "stdout");

    sidecar.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn crash_restarts_after_backoff() {
    if !node_available() {
        return;
    }
    let dir = temp_dir("backoff");
    let sidecar = Sidecar::new(config(&dir, "run")).unwrap();
    let _keep = sidecar.subscribe();
    sidecar.launch().unwrap();
    drive_until(|| sidecar.status().phase == RunPhase::Running).await;

    // Paused time auto-advances to the request timeout while the runtime waits
    // on the child pipe. A blocking sleep holds the clock still until the exit
    // is observed.
    let call = sidecar.call("test.crash", json!({}));
    tokio::pin!(call);
    let mut failed = None;
    for _ in 0..40 {
        tokio::select! {
            biased;
            result = &mut call => {
                failed = Some(result.unwrap_err());
                break;
            }
            _ = tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(30))) => {}
        }
    }
    let failed = failed.unwrap();
    assert!(matches!(failed, Error::Exited), "got {failed:?}");
    drive_until(|| sidecar.status().phase == RunPhase::BackingOff).await;
    assert_eq!(sidecar.status().spawn_count, 1);

    tokio::time::advance(RESTART_INITIAL_BACKOFF).await;
    drive_until(|| sidecar.status().spawn_count == 2).await;
    assert_eq!(sidecar.status().restarts_24h, 1);

    sidecar.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn crash_loop_stops_after_the_cap() {
    if !node_available() {
        return;
    }
    let dir = temp_dir("loop");
    let mut cfg = config(&dir, "crash");
    cfg.crash_loop_cap = 3;
    cfg.restart_initial = Duration::from_secs(1);
    cfg.restart_max = Duration::from_secs(5 * 60);
    let sidecar = Sidecar::new(cfg).unwrap();
    assert_eq!(sidecar.health().state, hub_core::ModuleState::Unknown);
    let _keep = sidecar.subscribe();
    sidecar.launch().unwrap();

    drive_until(|| {
        sidecar.status().phase == RunPhase::BackingOff && sidecar.status().spawn_count == 1
    })
    .await;
    tokio::time::advance(Duration::from_secs(1)).await;
    drive_until(|| {
        sidecar.status().phase == RunPhase::BackingOff && sidecar.status().spawn_count == 2
    })
    .await;
    tokio::time::advance(Duration::from_secs(2)).await;
    drive_until(|| {
        sidecar.status().phase == RunPhase::CrashLoop && sidecar.status().spawn_count == 3
    })
    .await;

    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    let status = sidecar.status();
    assert_eq!(status.spawn_count, 3);
    assert_eq!(status.restarts_24h, 2);
    assert!(status.crash_loop);
    assert_ne!(status.last_restart_unix_secs, 0);
    let health = sidecar.health();
    assert_eq!(health.state, hub_core::ModuleState::Down);
    assert!(health.detail.contains("crash_loop=1"));
    assert!(health.detail.contains("restarts_24h=2"));
    let err = sidecar.call("health", json!({})).await.unwrap_err();
    assert!(matches!(err, Error::CrashLoop));

    sidecar.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}
