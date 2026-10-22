//! Offline hub tests. The bot is a local fake and Zalo is the sidecar fixture.
//! Nothing here dials Telegram or Zalo, and nothing touches the real Keychain.

use std::net::TcpListener as StdListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use captain_vault::{MemoryStore, Profile, Secret};
use hub_analytics::health;
use hub_core::{ConversationId, ConversationKind, ModuleState, Network};
use hub_store::SharedHub;
use hub_web::HubApi;
use hub_zalo::RunPhase;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;

use super::{StoreHubApi, run_options, status_report, store_bot_token};
use crate::hub_run::{RunningHub, lock_hub, start_hub};
use crate::paths::Paths;

const TOKEN: &str = "999000:hub-test-token";
const GROUP: i64 = -100_123;
const WAIT: Duration = Duration::from_secs(15);
const LOCK_RETRY_ATTEMPTS: u32 = 50; // 50 x 20 ms covers a sidecar spawn between fork and exec
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

#[derive(Clone, Debug)]
struct Hit {
    method: String,
    status: u16,
    at: Instant,
    name: String,
}

struct BotState {
    hits: Vec<Hit>,
    updates: Vec<Value>,
    next_update: i64,
    next_topic: i32,
    next_message: i32,
    topic: i32,
    rate_left: u32,
    edit_rate_left: u32,
}

struct BotFake {
    base: String,
    state: Arc<Mutex<BotState>>,
    wake: Arc<Notify>,
}

impl BotFake {
    async fn start() -> Self {
        let listener = StdListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let state = Arc::new(Mutex::new(BotState {
            hits: Vec::new(),
            updates: Vec::new(),
            next_update: 1,
            next_topic: 11,
            next_message: 50,
            topic: 0,
            rate_left: 0,
            edit_rate_left: 0,
        }));
        let wake = Arc::new(Notify::new());
        let shared = Arc::clone(&state);
        let notify = Arc::clone(&wake);
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let shared = Arc::clone(&shared);
                let notify = Arc::clone(&notify);
                tokio::spawn(async move {
                    let _ = serve(socket, shared, notify).await;
                });
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            state,
            wake,
        }
    }

    async fn count(&self, method: &str, status: u16) -> usize {
        let state = Arc::clone(&self.state);
        let method = method.to_owned();
        tokio::task::spawn_blocking(move || {
            state
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .hits
                .iter()
                .filter(|hit| hit.method == method && hit.status == status)
                .count()
        })
        .await
        .unwrap_or(0)
    }

    fn topic(&self) -> i32 {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .topic
    }

    fn arm_rate_limit(&self) {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .rate_left = 1;
    }

    fn arm_edit_rate_limit(&self) {
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .edit_rate_left = 1;
    }

    async fn edits(&self) -> Vec<Hit> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            state
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .hits
                .iter()
                .filter(|hit| hit.method == "editForumTopic")
                .cloned()
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    async fn push_text(&self, thread: i32, text: &str) {
        let update = {
            let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
            let update_id = state.next_update;
            state.next_update += 1;
            json!({
                "update_id": update_id,
                "message": {
                    "message_id": 80 + update_id,
                    "message_thread_id": thread,
                    "from": {"id": 7, "is_bot": false, "first_name": "An"},
                    "date": 1_700_000_002,
                    "chat": {"id": GROUP, "type": "supergroup"},
                    "text": text
                }
            })
        };
        self.state
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .updates
            .push(update);
        self.wake.notify_one();
    }
}

async fn serve(
    mut socket: TcpStream,
    state: Arc<Mutex<BotState>>,
    wake: Arc<Notify>,
) -> std::io::Result<()> {
    let Some((path, body)) = read_request(&mut socket).await? else {
        return Ok(());
    };
    let method = path.rsplit('/').next().unwrap_or("").to_owned();
    if method == "getUpdates" {
        let offset = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| value.get("offset").and_then(Value::as_i64))
            .unwrap_or(0);
        let ready = async {
            loop {
                let waiting = {
                    let state = state.lock().unwrap_or_else(|err| err.into_inner());
                    state.updates.iter().any(|update| {
                        update.get("update_id").and_then(Value::as_i64).unwrap_or(0) >= offset
                    })
                };
                if waiting {
                    return;
                }
                tokio::select! {
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(250)) => return,
                }
            }
        };
        ready.await;
    }
    let name = request_name(&method, &body);
    let (status, response) = reply(&method, &state);
    {
        let mut state = state.lock().unwrap_or_else(|err| err.into_inner());
        state.hits.push(Hit {
            method,
            status,
            at: Instant::now(),
            name,
        });
    }
    let payload = response.into_bytes();
    let header = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(&payload).await?;
    Ok(())
}

fn reply(method: &str, state: &Mutex<BotState>) -> (u16, String) {
    let mut state = state.lock().unwrap_or_else(|err| err.into_inner());
    match method {
        "getMe" => (
            200,
            r#"{"ok":true,"result":{"id":42,"is_bot":true,"first_name":"Hub","username":"hub_bot"}}"#
                .to_owned(),
        ),
        "getUpdates" => {
            let batch: Vec<Value> = state.updates.drain(..).collect();
            (200, format!(r#"{{"ok":true,"result":{}}}"#, json!(batch)))
        }
        "createForumTopic" => {
            let topic = state.next_topic;
            state.next_topic += 1;
            state.topic = topic;
            (
                200,
                format!(
                    r#"{{"ok":true,"result":{{"message_thread_id":{topic},"name":"room-1","icon_color":7322096}}}}"#
                ),
            )
        }
        "editForumTopic" if state.edit_rate_left > 0 => {
            state.edit_rate_left -= 1;
            (
                429,
                r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":1}}"#
                    .to_owned(),
            )
        }
        "sendMessage" if state.rate_left > 0 => {
            state.rate_left -= 1;
            (
                429,
                r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":1}}"#
                    .to_owned(),
            )
        }
        "sendMessage" => {
            let id = state.next_message;
            state.next_message += 1;
            (
                200,
                format!(
                    r#"{{"ok":true,"result":{{"message_id":{id},"date":1700000000,"chat":{{"id":{GROUP},"type":"supergroup"}}}}}}"#
                ),
            )
        }
        _ => (200, r#"{"ok":true,"result":true}"#.to_owned()),
    }
}

async fn read_request(socket: &mut TcpStream) -> std::io::Result<Option<(String, Vec<u8>)>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = socket.read(&mut tmp).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
        let Some(header_end) = find_header_end(&buf) else {
            continue;
        };
        let header = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let length = content_length(&header);
        let body_at = header_end + 4;
        while buf.len() < body_at + length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let path = header
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_owned();
        let body = buf.get(body_at..body_at + length).unwrap_or(&[]).to_vec();
        return Ok(Some((path, body)));
    }
}

fn request_name(method: &str, body: &[u8]) -> String {
    if method != "editForumTopic" {
        return String::new();
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn content_length(header: &str) -> usize {
    header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn node_ok() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/hub-zalo/tests/fixtures/fake-sidecar.mjs")
}

fn temp_state(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("comms-hub-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_hub_toml(dir: &Path) {
    let script = fixture();
    let body = format!(
        "group_id = {GROUP}\nadmin_user_ids = [7]\nsidecar_program = \"node\"\nsidecar_args = [{:?}]\n",
        script.display().to_string()
    );
    std::fs::write(dir.join("hub.toml"), body).unwrap();
}

fn write_backfill_toml(dir: &Path) {
    let script = fixture();
    let body = format!(
        "group_id = {GROUP}\nadmin_user_ids = [7]\nsidecar_program = \"node\"\nsidecar_args = [{:?}]\navatar_card = false\nbackfill_rename_cap = 2\nbackfill_rename_gap_secs = 2\n",
        script.display().to_string()
    );
    std::fs::write(dir.join("hub.toml"), body).unwrap();
}

fn seed_numeric_topics(dir: &Path) {
    let hub = SharedHub::open(&dir.join("inbox.sqlite")).unwrap();
    let mut guard = hub.lock();
    for (external_id, topic) in [
        ("1001", 3703),
        ("1002", 3705),
        ("1003", 3707),
        ("1004", 3713),
        ("1005", 3718),
    ] {
        guard
            .upsert_topic_row(&group_id(external_id), topic, external_id, 1)
            .unwrap();
    }
}

fn group_id(external_id: &str) -> ConversationId {
    ConversationId {
        network: Network::Zalo,
        kind: ConversationKind::Group,
        external_id: external_id.to_owned(),
    }
}

fn stored_title(hub: &SharedHub, external_id: &str) -> String {
    hub.lock()
        .conversation_title(&group_id(external_id))
        .unwrap()
        .unwrap_or_default()
}

async fn wait_running(hub: &RunningHub) {
    let started = Instant::now();
    while hub.sidecar().status().phase != RunPhase::Running {
        assert!(started.elapsed() < WAIT, "sidecar did not reach running");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_count(fake: &BotFake, method: &str, status: u16, n: usize) {
    let started = Instant::now();
    while fake.count(method, status).await < n {
        assert!(
            started.elapsed() < WAIT,
            "timed out waiting for {method} status {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn emit_message(hub: &RunningHub, message_id: &str, text: &str) {
    hub.sidecar()
        .call(
            "test.emit",
            json!({
                "event": "message",
                "data": {
                    "conversation": "zalo:group:room-1",
                    "message_id": message_id,
                    "sender_id": "user-1",
                    "sender_display_name": "An",
                    "text": text,
                    "sent_at_unix_secs": 1_700_000_000
                }
            }),
        )
        .await
        .unwrap();
}

fn zalo_link(hub: &SharedHub, message_id: &str) -> Option<i64> {
    let id: ConversationId = "zalo:group:room-1".parse().unwrap();
    hub.lock()
        .message_link_for_network(&id, message_id)
        .unwrap()
}

async fn wait_link(hub: &SharedHub, message_id: &str) {
    let started = Instant::now();
    while zalo_link(hub, message_id).is_none() {
        assert!(started.elapsed() < WAIT, "zalo send was not mapped");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn round_trip_restart_crash_and_rate_limit_stay_offline() {
    if !node_ok() {
        return;
    }
    let dir = temp_state("round");
    write_hub_toml(&dir);
    let fake = BotFake::start().await;
    let running = start_hub(run_options(
        dir.clone(),
        Secret::new(TOKEN),
        fake.base.clone(),
        Some(Duration::from_millis(200)),
    ))
    .await
    .unwrap();
    wait_running(&running).await;
    running
        .sidecar()
        .call(
            "test.emit",
            json!({"event": "listener", "data": {"state": "up"}}),
        )
        .await
        .unwrap();
    running
        .sidecar()
        .call(
            "test.emit",
            json!({"event": "session", "data": {"state": "valid"}}),
        )
        .await
        .unwrap();
    emit_message(&running, "m-in-1", "hello from zalo").await;
    wait_count(&fake, "createForumTopic", 200, 1).await;
    wait_count(&fake, "sendMessage", 200, 1).await;
    let topic = fake.topic();
    assert!(topic > 0);
    fake.push_text(topic, "from topic").await;
    wait_link(running.hub(), "m-sent").await;

    running.shutdown().await.unwrap();
    let topics_before = 1;
    let running = start_hub(run_options(
        dir.clone(),
        Secret::new(TOKEN),
        fake.base.clone(),
        Some(Duration::from_millis(200)),
    ))
    .await
    .unwrap();
    wait_running(&running).await;
    emit_message(&running, "m-in-2", "again").await;
    wait_count(&fake, "sendMessage", 200, 2).await;
    assert_eq!(fake.count("createForumTopic", 200).await, topics_before);

    let sent_before = fake.count("sendMessage", 200).await;
    let _ = running.sidecar().call("test.crash", json!({})).await;
    let started = Instant::now();
    while running.sidecar().status().spawn_count < 2 {
        assert!(started.elapsed() < WAIT, "sidecar did not restart");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_running(&running).await;
    emit_message(&running, "m-in-3", "after crash").await;
    wait_count(&fake, "sendMessage", 200, sent_before + 1).await;

    fake.arm_rate_limit();
    emit_message(&running, "m-in-4", "after limit").await;
    wait_count(&fake, "sendMessage", 429, 1).await;
    wait_count(&fake, "sendMessage", 200, sent_before + 2).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let modules = StoreHubApi::new(running.hub().clone())
        .modules()
        .await
        .unwrap();
    assert_eq!(modules.modules.len(), 3);
    assert_eq!(modules.modules[0].module, "captain-daemon");
    assert_eq!(modules.modules[0].state, ModuleState::Unknown);
    assert!(
        modules
            .modules
            .iter()
            .any(|row| { row.module == "zalo-bridge" && row.state != ModuleState::Unknown })
    );
    assert!(
        modules
            .modules
            .iter()
            .any(|row| row.module == "bridge" && row.state != ModuleState::Unknown)
    );
    let events = StoreHubApi::new(running.hub().clone())
        .events(50)
        .await
        .unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|event| event.kind == "listener_drop" || event.kind == "reconnect")
    );
    let hub = running.hub().clone();
    let report = tokio::task::spawn_blocking(move || {
        let guard = hub.lock();
        let now = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .unwrap_or(0);
        health(&guard.reader(), now)
    })
    .await
    .unwrap()
    .unwrap();
    assert!(report.modules.iter().any(|row| row.module == "bridge"));
    assert!(report.modules.iter().any(|row| row.module == "zalo-bridge"));

    let shown = format!("{:?}", running.shutdown().await);
    assert!(!shown.contains(TOKEN));
    let status = std::fs::read_to_string(dir.join("hub.toml")).unwrap_or_default();
    assert!(!status.contains(TOKEN));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn directory_backfill_renames_four_of_five_and_carries_the_cap() {
    if !node_ok() {
        return;
    }
    const PHASE: Duration = Duration::from_secs(30);
    let dir = temp_state("backfill");
    write_backfill_toml(&dir);
    seed_numeric_topics(&dir);
    let fake = BotFake::start().await;
    let options = || {
        run_options(
            dir.clone(),
            Secret::new(TOKEN),
            fake.base.clone(),
            Some(Duration::from_millis(200)),
        )
    };

    let running = start_hub(options()).await.unwrap();
    wait_running(&running).await;
    emit_directory(&running).await;
    wait_count_for(&fake, "editForumTopic", 200, 1, PHASE).await;
    fake.arm_edit_rate_limit();
    wait_count_for(&fake, "editForumTopic", 429, 1, PHASE).await;
    wait_count_for(&fake, "editForumTopic", 200, 2, PHASE).await;
    let first = fake.edits().await;
    assert_eq!(first.len(), 3, "{first:?}");
    assert_eq!(first[0].status, 200);
    assert!(first[0].name.contains("An"), "{}", first[0].name);
    assert_eq!(first[1].status, 429);
    assert!(first[1].name.contains("Binh"), "{}", first[1].name);
    assert!(first[2].name.contains("Binh"), "{}", first[2].name);
    assert!(first[1].at.saturating_duration_since(first[0].at) >= Duration::from_secs(1));
    assert_eq!(stored_title(running.hub(), "1001"), "An");
    assert_eq!(stored_title(running.hub(), "1002"), "Binh");
    assert_eq!(stored_title(running.hub(), "1003"), "1003");
    assert_eq!(stored_title(running.hub(), "1004"), "1004");
    assert_eq!(stored_title(running.hub(), "1005"), "1005");
    assert_eq!(fake.count("createForumTopic", 200).await, 0);
    running.shutdown().await.unwrap();

    let running = start_hub(options()).await.unwrap();
    wait_running(&running).await;
    emit_directory(&running).await;
    wait_count_for(&fake, "editForumTopic", 200, 4, PHASE).await;
    let second = fake.edits().await;
    assert_eq!(second.len(), 5, "{second:?}");
    assert!(second[3].name.contains("Chi"), "{}", second[3].name);
    assert!(second[4].name.contains("Dung"), "{}", second[4].name);
    assert!(second[4].at.saturating_duration_since(second[3].at) >= Duration::from_secs(1));
    assert_eq!(stored_title(running.hub(), "1003"), "Chi");
    assert_eq!(stored_title(running.hub(), "1004"), "Dung");
    assert_eq!(stored_title(running.hub(), "1005"), "1005");
    assert_eq!(fake.count("editForumTopic", 429).await, 1);
    running.shutdown().await.unwrap();

    let running = start_hub(options()).await.unwrap();
    wait_running(&running).await;
    emit_directory(&running).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(fake.count("editForumTopic", 200).await, 4);
    assert_eq!(fake.count("editForumTopic", 429).await, 1);
    assert_eq!(fake.count("createForumTopic", 200).await, 0);
    assert_eq!(stored_title(running.hub(), "1001"), "An");
    assert_eq!(stored_title(running.hub(), "1005"), "1005");
    assert!(
        fake.edits()
            .await
            .iter()
            .all(|hit| !hit.name.contains("1005"))
    );
    running.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

async fn wait_count_for(fake: &BotFake, method: &str, status: u16, n: usize, limit: Duration) {
    let started = Instant::now();
    while fake.count(method, status).await < n {
        assert!(
            started.elapsed() < limit,
            "timed out waiting for {method} status {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn emit_directory(hub: &RunningHub) {
    hub.sidecar()
        .call(
            "test.emit",
            json!({
                "event": "directory",
                "data": {
                    "entries": [
                        {"kind": "group", "id": "1001", "name": "An"},
                        {"kind": "group", "id": "1002", "name": "Binh"},
                        {"kind": "group", "id": "1003", "name": "Chi"},
                        {"kind": "group", "id": "1004", "name": "Dung"}
                    ]
                }
            }),
        )
        .await
        .unwrap();
}

#[test]
fn a_second_hub_is_refused_while_the_lock_is_held() {
    let dir = temp_state("lock");
    let first = lock_hub(&dir).unwrap();
    let err = lock_hub(&dir).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("another hub process"), "{text}");
    drop(first);
    // A sidecar another test is spawning can hold a copy of the descriptor between fork and
    // exec, which keeps the flock for a few milliseconds after the drop.
    let mut second = lock_hub(&dir);
    for _ in 0..LOCK_RETRY_ATTEMPTS {
        if second.is_ok() {
            break;
        }
        std::thread::sleep(LOCK_RETRY_DELAY);
        second = lock_hub(&dir);
    }
    drop(second.unwrap());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn token_stays_out_of_status_and_errors() {
    let dir = temp_state("token");
    let paths = Paths::new(&dir);
    let profile = Profile::new("default").unwrap();
    let store = MemoryStore::default();
    let missing = status_report(&store, &profile, &paths).unwrap();
    assert_eq!(missing["bot_token"], "missing");
    assert!(!paths.store().is_file());
    let secret = Secret::new(TOKEN);
    store_bot_token(&store, &profile, &secret).unwrap();
    let report = status_report(&store, &profile, &paths).unwrap();
    let text = report.to_string();
    assert_eq!(report["bot_token"], "stored");
    assert!(!text.contains(TOKEN));
    assert!(!format!("{report:?}").contains(TOKEN));
    let empty = store_bot_token(&store, &profile, &Secret::new("")).unwrap_err();
    assert_eq!(empty.to_string(), "bot token is empty");
    assert!(!format!("{empty:?}").contains(TOKEN));
    let rejected = crate::hub_config::HubConfig::parse(&format!(
        "bot_token = \"{TOKEN}\"\ngroup_id = -1001\n"
    ))
    .unwrap_err();
    assert!(rejected.to_string().starts_with("hub.toml:"));
    assert!(!rejected.to_string().contains(TOKEN));
    assert!(!format!("{rejected:?}").contains(TOKEN));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn qr_png_is_private_and_drawn_as_blocks() {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 255, 255, 0]).unwrap();
    }
    let dir = temp_state("qr");
    let path = super::write_qr_png(&dir, &bytes).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(super::render_qr_blocks(&bytes).unwrap(), "▀▄");
    let encoded = base64_encode(&bytes);
    assert_eq!(super::decode_base64(&encoded).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(dir);
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let n = (u32::from(bytes[index]) << 16)
            | (u32::from(bytes[index + 1]) << 8)
            | u32::from(bytes[index + 2]);
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        out.push(ALPHA[((n >> 6) & 63) as usize] as char);
        out.push(ALPHA[(n & 63) as usize] as char);
        index += 3;
    }
    if index < bytes.len() {
        let left = bytes.len() - index;
        let mut n = u32::from(bytes[index]) << 16;
        if left == 2 {
            n |= u32::from(bytes[index + 1]) << 8;
        }
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        if left == 2 {
            out.push(ALPHA[((n >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

#[test]
fn hub_idle_loop_adds_no_interval() {
    let idle = include_str!("hub_run.rs")
        .matches("tokio::time::interval(")
        .count();
    assert_eq!(idle, 3);
    let book = include_str!("../../../sidecars/zalo-node/src/zalo/directory-book.ts");
    assert!(!book.contains("setInterval"));
    let adapter = include_str!("../../../sidecars/zalo-node/src/zalo/adapter.ts");
    assert!(!adapter.contains("setInterval"));
}
