//! Offline tests against [`crate::fake`]. Nothing here dials Telegram.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use frankenstein::response::MethodResponse;
use frankenstein::types::{ChatFullInfo, ChatMember, Message, User};
use frankenstein::updates::Update;
use hub_core::{Attachment, AttachmentKind, AttachmentSource, TgMessageId, TopicId, TopicSink};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::fake::{Fake, Hit, Reply};
use crate::limits::{self, LONG_POLL_TIMEOUT_SECS};
use crate::{
    BotClient, BotConfig, BotRight, BotToken, Error, IncomingMessage, IncomingReaction,
    OffsetStore, PreflightReport, PreflightVerdict, TopicIconColor, TopicIncoming, TopicOptions,
    TransferDirection,
};

const TOKEN: &str = "123:test";
const GROUP_ID: i64 = -100_123;
const THREAD: i32 = 7;
const BOT_ID: u64 = 42;

fn checked<T: DeserializeOwned>(body: String) -> String {
    let _: T = serde_json::from_str(&body).unwrap();
    body
}

fn envelope<T: DeserializeOwned>(result: &str) -> String {
    checked::<MethodResponse<T>>(format!(r#"{{"ok":true,"result":{result}}}"#))
}

fn ok_message(id: i32) -> String {
    envelope::<Message>(&format!(
        r#"{{"message_id":{id},"date":1700000000,"chat":{{"id":{GROUP_ID},"type":"supergroup"}}}}"#
    ))
}

fn ok_messages(ids: &[i32]) -> String {
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(
                r#"{{"message_id":{id},"date":1700000000,"chat":{{"id":{GROUP_ID},"type":"supergroup"}}}}"#
            )
        })
        .collect();
    envelope::<Vec<Message>>(&format!("[{}]", items.join(",")))
}

fn ok_bool() -> String {
    envelope::<bool>("true")
}

fn ok_topic() -> String {
    envelope::<frankenstein::types::ForumTopic>(&format!(
        r#"{{"message_thread_id":{THREAD},"name":"Room","icon_color":{}}}"#,
        limits::ICON_COLOR_BLUE
    ))
}

fn api_error(code: u64, description: &str, retry_after: Option<u16>) -> String {
    match retry_after {
        Some(secs) => format!(
            r#"{{"ok":false,"description":"{description}","error_code":{code},"parameters":{{"retry_after":{secs}}}}}"#
        ),
        None => format!(r#"{{"ok":false,"description":"{description}","error_code":{code}}}"#),
    }
}

fn test_client(base: &str) -> BotClient {
    BotClient::new(BotConfig {
        token: BotToken::new(TOKEN),
        api_base_url: base.to_owned(),
        group_id: GROUP_ID,
    })
    .unwrap()
}

fn json_body(hit: &Hit) -> Value {
    serde_json::from_slice(&hit.body).unwrap()
}

fn assert_json(hit: &Hit, method: &str) {
    assert_eq!(hit.method(), method);
    assert!(
        hit.content_type.starts_with("application/json"),
        "{}",
        hit.content_type
    );
    assert!(hit.path.contains(TOKEN) || hit.path.contains(&TOKEN.replace(':', "%3A")));
}

fn url_file(kind: AttachmentKind, url: &str) -> Attachment {
    Attachment {
        kind,
        mime: None,
        size_bytes: None,
        source: AttachmentSource::Url(url.to_owned()),
        file_name: None,
    }
}

fn reply_for(hit: &Hit) -> Reply {
    let body = match hit.method() {
        "createForumTopic" => ok_topic(),
        "closeForumTopic" | "reopenForumTopic" | "deleteMessage" | "setMessageReaction" => {
            ok_bool()
        }
        "editMessageText" => ok_bool(),
        "sendMediaGroup" => ok_messages(&[100, 101]),
        _ => ok_message(50),
    };
    Reply::json(200, body)
}

#[tokio::test]
async fn request_shapes_cover_each_method() {
    let fake = Fake::bind(reply_for).await;
    let client = test_client(&fake.base);
    let thread = TopicId(THREAD);

    client.create_topic("Plain".into()).await.unwrap();
    client
        .create_topic_with(
            "Room".into(),
            TopicOptions {
                icon_color: Some(TopicIconColor::Blue),
                icon_custom_emoji_id: Some("emoji-1".into()),
            },
        )
        .await
        .unwrap();
    client.close_topic(thread).await.unwrap();
    client.reopen_topic(thread).await.unwrap();
    client
        .send_text(thread, "<b>hi</b>".into(), Some(TgMessageId(3)))
        .await
        .unwrap();

    let kinds = [
        (
            AttachmentKind::Photo,
            "sendPhoto",
            "photo",
            "https://cdn.example/a.jpg",
        ),
        (
            AttachmentKind::Video,
            "sendVideo",
            "video",
            "https://cdn.example/a.mp4",
        ),
        (
            AttachmentKind::Audio,
            "sendAudio",
            "audio",
            "https://cdn.example/a.mp3",
        ),
        (
            AttachmentKind::Voice,
            "sendVoice",
            "voice",
            "https://cdn.example/a.ogg",
        ),
        (
            AttachmentKind::File,
            "sendDocument",
            "document",
            "https://cdn.example/a.bin",
        ),
        (
            AttachmentKind::Gif,
            "sendAnimation",
            "animation",
            "https://cdn.example/a.mp4",
        ),
    ];
    for (kind, _method, _field, url) in kinds {
        client
            .send_media(thread, url_file(kind, url), Some("cap".into()))
            .await
            .unwrap();
    }
    let mut sticker = url_file(AttachmentKind::Sticker, "https://cdn.example/s.webp");
    sticker.mime = Some("image/webp".into());
    client.send_media(thread, sticker, None).await.unwrap();
    let mut animated = url_file(AttachmentKind::Sticker, "https://cdn.example/s.tgs");
    animated.mime = Some("application/x-tgsticker".into());
    client.send_media(thread, animated, None).await.unwrap();

    let album = client
        .send_media_group(
            thread,
            vec![
                url_file(AttachmentKind::Photo, "https://cdn.example/1.jpg"),
                url_file(AttachmentKind::Photo, "https://cdn.example/2.jpg"),
            ],
            Some("album".into()),
        )
        .await
        .unwrap();
    assert_eq!(album, vec![TgMessageId(100), TgMessageId(101)]);
    client
        .send_media_group(
            thread,
            vec![url_file(
                AttachmentKind::Photo,
                "https://cdn.example/only.jpg",
            )],
            Some("one".into()),
        )
        .await
        .unwrap();

    client
        .set_reaction(TopicId(99), TgMessageId(11), Some("👍".into()))
        .await
        .unwrap();
    client
        .set_reaction(thread, TgMessageId(11), None)
        .await
        .unwrap();
    client
        .delete_message(TopicId(99), TgMessageId(11))
        .await
        .unwrap();

    let hits = fake.hits().await;
    let methods: Vec<&str> = hits.iter().map(Hit::method).collect();
    assert_eq!(hits.len(), 18, "{methods:?}");

    assert_json(&hits[0], "createForumTopic");
    let body = json_body(&hits[0]);
    assert_eq!(body["chat_id"], GROUP_ID);
    assert_eq!(body["name"], "Plain");
    assert!(body.get("icon_color").is_none());

    let body = json_body(&hits[1]);
    assert_eq!(body["name"], "Room");
    assert_eq!(body["icon_color"], limits::ICON_COLOR_BLUE);
    assert_eq!(body["icon_custom_emoji_id"], "emoji-1");

    for (index, method) in [(2, "closeForumTopic"), (3, "reopenForumTopic")] {
        assert_json(&hits[index], method);
        assert_eq!(json_body(&hits[index])["message_thread_id"], THREAD);
    }

    assert_json(&hits[4], "sendMessage");
    let body = json_body(&hits[4]);
    assert_eq!(body["text"], "<b>hi</b>");
    assert_eq!(body["parse_mode"], "HTML");
    assert_eq!(body["message_thread_id"], THREAD);
    assert_eq!(body["chat_id"], GROUP_ID);
    assert_eq!(body["reply_parameters"]["message_id"], 3);
    assert_eq!(
        body["reply_parameters"]["allow_sending_without_reply"],
        true
    );

    let media = [
        (5, "sendPhoto", "photo", "https://cdn.example/a.jpg"),
        (6, "sendVideo", "video", "https://cdn.example/a.mp4"),
        (7, "sendAudio", "audio", "https://cdn.example/a.mp3"),
        (8, "sendVoice", "voice", "https://cdn.example/a.ogg"),
        (9, "sendDocument", "document", "https://cdn.example/a.bin"),
        (
            10,
            "sendAnimation",
            "animation",
            "https://cdn.example/a.mp4",
        ),
    ];
    for (index, method, field, url) in media {
        assert_json(&hits[index], method);
        let body = json_body(&hits[index]);
        assert_eq!(body[field], url);
        assert_eq!(body["caption"], "cap");
        assert_eq!(body["message_thread_id"], THREAD);
        assert!(body.get("parse_mode").is_none());
    }

    assert_json(&hits[11], "sendPhoto");
    assert_eq!(json_body(&hits[11])["photo"], "https://cdn.example/s.webp");
    assert_json(&hits[12], "sendDocument");
    assert_eq!(
        json_body(&hits[12])["document"],
        "https://cdn.example/s.tgs"
    );

    assert_json(&hits[13], "sendMediaGroup");
    let body = json_body(&hits[13]);
    assert_eq!(body["media"].as_array().unwrap().len(), 2);
    assert_eq!(body["media"][0]["type"], "photo");
    assert_eq!(body["media"][0]["media"], "https://cdn.example/1.jpg");
    assert_eq!(body["media"][0]["caption"], "album");
    assert!(body["media"][1].get("caption").is_none());
    assert_eq!(body["message_thread_id"], THREAD);

    assert_json(&hits[14], "sendPhoto");
    assert_eq!(
        json_body(&hits[14])["photo"],
        "https://cdn.example/only.jpg"
    );
    assert_eq!(json_body(&hits[14])["caption"], "one");

    assert_json(&hits[15], "setMessageReaction");
    let body = json_body(&hits[15]);
    assert_eq!(body["message_id"], 11);
    assert_eq!(body["reaction"][0]["type"], "emoji");
    assert_eq!(body["reaction"][0]["emoji"], "👍");
    assert!(body.get("message_thread_id").is_none());
    assert!(!hits[15].text().contains("\"99\""));

    assert_json(&hits[16], "setMessageReaction");
    assert_eq!(json_body(&hits[16])["reaction"], json!([]));

    assert_json(&hits[17], "deleteMessage");
    let body = json_body(&hits[17]);
    assert_eq!(body["message_id"], 11);
    assert!(body.get("message_thread_id").is_none());
}

#[tokio::test]
async fn edit_message_sends_html_without_a_thread() {
    let fake = Fake::bind(|_| Reply::json(200, ok_bool())).await;
    let client = test_client(&fake.base);
    client
        .edit_message_text(TgMessageId(12), "<b>n</b>".into())
        .await
        .unwrap();
    let hits = fake.hits().await;
    assert_json(&hits[0], "editMessageText");
    let body = json_body(&hits[0]);
    assert_eq!(body["message_id"], 12);
    assert_eq!(body["text"], "<b>n</b>");
    assert_eq!(body["parse_mode"], "HTML");
    assert!(body.get("message_thread_id").is_none());
}

#[tokio::test]
async fn local_uploads_use_multipart_and_sticker_rules() {
    let dir = std::env::temp_dir().join(format!("hub-tgbot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let photo = dir.join("photo.jpg");
    let still = dir.join("still.webp");
    let video = dir.join("move.webm");
    let one = dir.join("one.jpg");
    let two = dir.join("two.jpg");
    for path in [&photo, &still, &video, &one, &two] {
        std::fs::write(path, b"bytes").unwrap();
    }

    let fake = Fake::bind(|hit| match hit.method() {
        "sendMediaGroup" => Reply::json(200, ok_messages(&[1, 2])),
        _ => Reply::json(200, ok_message(3)),
    })
    .await;
    let client = test_client(&fake.base);
    let thread = TopicId(THREAD);

    client
        .send_media(
            thread,
            local_file(AttachmentKind::Photo, &photo, None),
            Some("c".into()),
        )
        .await
        .unwrap();
    client
        .send_media(
            thread,
            local_file(AttachmentKind::Sticker, &still, Some("image/webp")),
            None,
        )
        .await
        .unwrap();
    client
        .send_media(
            thread,
            local_file(AttachmentKind::Sticker, &video, None),
            None,
        )
        .await
        .unwrap();
    client
        .send_media_group(
            thread,
            vec![
                local_file(AttachmentKind::Photo, &one, None),
                local_file(AttachmentKind::Photo, &two, None),
            ],
            Some("pair".into()),
        )
        .await
        .unwrap();

    let hits = fake.hits().await;
    assert_eq!(hits[0].method(), "sendPhoto");
    assert!(hits[0].content_type.starts_with("multipart/form-data"));
    let text = hits[0].text();
    assert!(text.contains("name=\"photo\""));
    assert!(text.contains("filename=\"photo.jpg\""));
    assert!(text.contains("name=\"message_thread_id\""));
    assert!(text.contains("name=\"caption\""));
    assert!(!text.contains("parse_mode"));

    assert_eq!(hits[1].method(), "sendPhoto");
    assert!(hits[1].text().contains("filename=\"still.webp\""));
    assert_eq!(hits[2].method(), "sendDocument");
    assert!(hits[2].text().contains("filename=\"move.webm\""));

    assert_eq!(hits[3].method(), "sendMediaGroup");
    let album = hits[3].text();
    assert!(album.contains("attach://file0"));
    assert!(album.contains("attach://file1"));
    assert!(album.contains("filename=\"one.jpg\"") || album.contains("name=\"file0\""));
    assert!(album.contains("pair"));

    let _ = std::fs::remove_dir_all(&dir);
}

fn local_file(kind: AttachmentKind, path: &Path, mime: Option<&str>) -> Attachment {
    Attachment {
        kind,
        mime: mime.map(str::to_owned),
        size_bytes: None,
        source: AttachmentSource::LocalPath(path.display().to_string()),
        file_name: None,
    }
}

#[tokio::test]
async fn album_of_eleven_photos_splits_and_keeps_the_caption_on_the_first() {
    let fake = Fake::bind(|hit| match hit.method() {
        "sendMediaGroup" => Reply::json(200, ok_messages(&(0..10).collect::<Vec<_>>())),
        "sendPhoto" => Reply::json(200, ok_message(10)),
        _ => Reply::json(500, api_error(500, "unexpected", None)),
    })
    .await;
    let photos: Vec<Attachment> = (0..11)
        .map(|index| {
            url_file(
                AttachmentKind::Photo,
                &format!("https://cdn.example/{index}.jpg"),
            )
        })
        .collect();
    let ids = test_client(&fake.base)
        .send_media_group(TopicId(THREAD), photos, Some("cap".into()))
        .await
        .unwrap();
    assert_eq!(ids.len(), 11);
    let hits = fake.hits().await;
    assert_eq!(hits.len(), 2);
    let album = json_body(&hits[0]);
    assert_eq!(hits[0].method(), "sendMediaGroup");
    assert_eq!(album["media"].as_array().unwrap().len(), 10);
    assert_eq!(album["media"][0]["caption"], "cap");
    assert!(album["media"][1].get("caption").is_none());
    let last = json_body(&hits[1]);
    assert_eq!(hits[1].method(), "sendPhoto");
    assert_eq!(last["photo"], "https://cdn.example/10.jpg");
    assert!(last.get("caption").is_none());
}

#[tokio::test]
async fn oversized_and_non_files_fail_before_http() {
    let client = test_client("http://127.0.0.1:1");
    let thread = TopicId(THREAD);

    let mut download = url_file(AttachmentKind::Photo, "https://cdn.example/big.jpg");
    download.size_bytes = Some(limits::DOWNLOAD_MAX_BYTES + 1);
    let err = client.send_media(thread, download, None).await.unwrap_err();
    assert_eq!(
        err,
        Error::TooLarge {
            direction: TransferDirection::Download,
            size_bytes: limits::DOWNLOAD_MAX_BYTES + 1,
            limit_bytes: limits::DOWNLOAD_MAX_BYTES,
        }
    );

    let upload = Attachment {
        kind: AttachmentKind::File,
        mime: None,
        size_bytes: Some(limits::UPLOAD_MAX_BYTES + 1),
        source: AttachmentSource::LocalPath("/tmp/hub-tgbot-not-created.bin".into()),
        file_name: None,
    };
    let err = TopicSink::send_media(&client, thread, upload, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        hub_core::Error::Adapter { detail } if detail.starts_with("too_large upload")
    ));

    let missing = Attachment {
        kind: AttachmentKind::Photo,
        mime: None,
        size_bytes: None,
        source: AttachmentSource::LocalPath("/tmp/hub-tgbot-missing-file".into()),
        file_name: None,
    };
    assert!(matches!(
        client.send_media(thread, missing, None).await.unwrap_err(),
        Error::Permanent { status: None }
    ));
    assert!(matches!(
        client
            .send_media(
                thread,
                url_file(AttachmentKind::Poll, "https://cdn.example/p"),
                None
            )
            .await
            .unwrap_err(),
        Error::Permanent { status: None }
    ));
    assert!(matches!(
        client
            .send_media_group(thread, Vec::new(), None)
            .await
            .unwrap_err(),
        Error::Permanent { status: None }
    ));
    assert!(matches!(
        client
            .send_media_group(
                thread,
                vec![url_file(AttachmentKind::Video, "https://cdn.example/v.mp4")],
                None,
            )
            .await
            .unwrap_err(),
        Error::Permanent { status: None }
    ));
}

#[tokio::test]
async fn http_errors_map_without_retrying_non_429() {
    let cases = [
        (
            400,
            "Bad Request: message thread not found",
            Error::TopicGone,
        ),
        (400, "Bad Request: TOPIC_DELETED", Error::TopicGone),
        (400, "Bad Request: TOPIC_CLOSED", Error::TopicGone),
        (
            403,
            "Forbidden: bot was blocked by the user",
            Error::LacksRights,
        ),
        (
            400,
            "Bad Request: not enough rights to manage topics",
            Error::LacksRights,
        ),
        (
            400,
            "Bad Request: have no rights to send a message",
            Error::LacksRights,
        ),
        (400, "Bad Request: CHAT_ADMIN_REQUIRED", Error::LacksRights),
        (
            500,
            "Internal Server Error",
            Error::Transient { status: Some(500) },
        ),
        (
            408,
            "Request Timeout",
            Error::Transient { status: Some(408) },
        ),
        (
            400,
            "Bad Request: message text is empty",
            Error::Permanent { status: Some(400) },
        ),
        (401, "Unauthorized", Error::Permanent { status: Some(401) }),
    ];
    let queue = Arc::new(StdMutex::new(
        cases
            .iter()
            .map(|(code, description, _)| {
                Reply::json(*code, api_error(u64::from(*code), description, None))
            })
            .collect::<Vec<_>>(),
    ));
    let fake = Fake::bind(move |_| queue.lock().unwrap().remove(0)).await;
    let client = test_client(&fake.base);
    for (_, _, expected) in &cases {
        let err = client
            .send_text(TopicId(THREAD), "x".into(), None)
            .await
            .unwrap_err();
        assert_eq!(err, *expected);
    }
    assert_eq!(fake.hit_count().await, cases.len());
}

#[tokio::test]
async fn topic_gone_and_false_edit_cross_the_trait_boundary() {
    let queue = Arc::new(StdMutex::new(Vec::from([
        Reply::json(400, api_error(400, "Bad Request: TOPIC_DELETED", None)),
        Reply::json(200, r#"{"ok":true,"result":false}"#),
    ])));
    let fake = Fake::bind(move |_| queue.lock().unwrap().remove(0)).await;
    let client = test_client(&fake.base);
    let err = TopicSink::close_topic(&client, TopicId(THREAD))
        .await
        .unwrap_err();
    assert_eq!(err, hub_core::Error::TopicGone);
    let err = client
        .edit_message_text(TgMessageId(1), "x".into())
        .await
        .unwrap_err();
    assert_eq!(err, Error::Permanent { status: None });
}

#[tokio::test]
async fn malformed_json_is_permanent_and_omits_the_body() {
    let body = format!("{{not-json {TOKEN}");
    let fake = Fake::bind(move |_| Reply::json(200, body.clone())).await;
    let err = test_client(&fake.base)
        .send_text(TopicId(THREAD), "x".into(), None)
        .await
        .unwrap_err();
    assert_eq!(err, Error::Permanent { status: None });
    let rendered = format!("{err} {err:?} {}", err.adapter_detail());
    assert!(!rendered.contains(TOKEN));
}

#[tokio::test]
async fn slow_response_is_waited_out() {
    let fake = Fake::bind(|_| Reply::delayed(200, ok_message(1), Duration::from_millis(200))).await;
    let started = std::time::Instant::now();
    test_client(&fake.base)
        .send_text(TopicId(THREAD), "x".into(), None)
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
}

#[tokio::test]
async fn concurrency_cap_is_five() {
    let fake = Fake::bind(|_| Reply::delayed(200, ok_message(1), Duration::from_millis(150))).await;
    let client = test_client(&fake.base);
    let mut joins = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        joins.push(tokio::spawn(async move {
            client
                .send_text(TopicId(THREAD), "x".into(), None)
                .await
                .unwrap();
        }));
    }
    for join in joins {
        join.await.unwrap();
    }
    assert_eq!(fake.max_inflight(), limits::REQUEST_CONCURRENCY);
    let hits = fake.hits().await;
    assert_eq!(hits.len(), 8);
    let gap = hits[5].at.saturating_duration_since(hits[0].at);
    assert!(
        gap >= Duration::from_millis(80),
        "sixth request started after {}ms",
        gap.as_millis()
    );
}

#[tokio::test]
async fn long_poll_does_not_take_a_send_slot() {
    let me = envelope::<User>(BOT_USER);
    let fake = Fake::bind(move |hit| match hit.method() {
        "getUpdates" => Reply::hang(),
        "getMe" => Reply::json(200, me.clone()),
        _ => Reply::delayed(200, ok_message(1), Duration::from_millis(200)),
    })
    .await;
    let client = test_client(&fake.base);
    let offsets = MemOffsets {
        value: Arc::new(StdMutex::new(None)),
    };
    let (tx, rx) = mpsc::channel(4);
    let (stop_tx, stop_rx) = watch::channel(false);
    let poller = client.clone();
    let poll = tokio::spawn(async move { poller.run_updates(&offsets, tx, stop_rx).await });
    fake.wait_completed(2).await;
    let mut joins = Vec::new();
    for _ in 0..5 {
        let client = client.clone();
        joins.push(tokio::spawn(async move {
            client
                .send_text(TopicId(THREAD), "x".into(), None)
                .await
                .unwrap();
        }));
    }
    for join in joins {
        join.await.unwrap();
    }
    let hits = fake.hits().await;
    let sends: Vec<_> = hits
        .iter()
        .filter(|hit| hit.method() == "sendMessage")
        .collect();
    assert_eq!(sends.len(), 5);
    let gap = sends[4].at.saturating_duration_since(sends[0].at);
    assert!(
        gap < Duration::from_millis(120),
        "fifth send started {}ms after the first",
        gap.as_millis()
    );
    let _ = stop_tx.send(true);
    drop(rx);
    let _ = poll.await;
}

#[tokio::test(start_paused = true)]
async fn global_pause_blocks_other_calls_until_retry_after_plus_one() {
    let seen = Arc::new(AtomicUsize::new(0));
    let seen2 = Arc::clone(&seen);
    let fake = Fake::bind(move |_| {
        let n = seen2.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Reply::json(429, api_error(429, "Too Many Requests", Some(2)))
        } else {
            Reply::json(200, ok_message(i32::try_from(n).unwrap_or(1)))
        }
    })
    .await;
    let client = test_client(&fake.base);
    let first = tokio::spawn({
        let client = client.clone();
        async move { client.send_text(TopicId(THREAD), "a".into(), None).await }
    });
    // Paused time does not wake a socket by itself. Short advances let the
    // runtime poll the fake without spending the 3s pause.
    drive_until_completed(&fake, 1).await;
    drive_for(Duration::from_millis(200)).await;
    let second = tokio::spawn({
        let client = client.clone();
        async move { client.send_text(TopicId(THREAD), "b".into(), None).await }
    });
    drive_for(Duration::from_millis(200)).await;
    assert_eq!(fake.hit_count().await, 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    drive_for(Duration::from_millis(50)).await;
    assert_eq!(fake.hit_count().await, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    drive_until_completed(&fake, 3).await;
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
    assert_eq!(seen.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn rate_limit_returns_after_five_retries() {
    let fake = Fake::bind(|_| Reply::json(429, api_error(429, "Too Many Requests", Some(1)))).await;
    let client = test_client(&fake.base);
    let task = tokio::spawn(async move {
        TopicSink::send_text(&client, TopicId(THREAD), "a".into(), None).await
    });
    for n in 1..=6 {
        drive_until_completed(&fake, n).await;
        drive_for(Duration::from_millis(100)).await;
        assert_eq!(fake.hit_count().await, n);
        if n < 6 {
            tokio::time::advance(Duration::from_millis(500)).await;
            drive_for(Duration::from_millis(50)).await;
            assert_eq!(fake.hit_count().await, n);
            tokio::time::advance(Duration::from_millis(1500)).await;
        }
    }
    let err = task.await.unwrap().unwrap_err();
    assert_eq!(
        err,
        hub_core::Error::RateLimited {
            retry_after_secs: 1
        }
    );
    assert_eq!(fake.hit_count().await, 6);
}

/// Moves paused time in small steps so socket tasks get polled.
async fn drive_for(total: Duration) {
    let step = Duration::from_millis(5);
    let mut left = total;
    while !left.is_zero() {
        let chunk = step.min(left);
        tokio::time::advance(chunk).await;
        left -= chunk;
        tokio::task::yield_now().await;
    }
}

async fn drive_until_completed(fake: &Fake, n: usize) {
    for _ in 0..400 {
        if fake.completed_count() >= n {
            return;
        }
        tokio::time::advance(Duration::from_millis(5)).await;
    }
    assert!(
        fake.completed_count() >= n,
        "fake completed {} of {n}",
        fake.completed_count()
    );
}

#[tokio::test]
#[allow(clippy::panic)]
async fn updates_parse_text_photo_voice_reply_and_reaction() {
    let updates = sample_updates();
    let _: MethodResponse<Vec<Update>> = serde_json::from_str(&updates).unwrap();
    let me = envelope::<User>(r#"{"id":42,"is_bot":true,"first_name":"Hub","username":"hub_bot"}"#);
    let polls = Arc::new(AtomicUsize::new(0));
    let polls2 = Arc::clone(&polls);
    let fake = Fake::bind(move |hit| match hit.method() {
        "getMe" => Reply::json(200, me.clone()),
        "getUpdates" => {
            if polls2.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::json(200, updates.clone())
            } else {
                Reply::hang()
            }
        }
        _ => Reply::json(500, api_error(500, "unexpected", None)),
    })
    .await;
    let client = test_client(&fake.base);
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let store = Arc::new(StdMutex::new(None::<i64>));
    let stored = Arc::clone(&store);
    let run = tokio::spawn(async move {
        let offsets = MemOffsets { value: stored };
        client.run_updates(&offsets, events_tx, shutdown_rx).await
    });
    let mut events = Vec::new();
    while events.len() < 6 {
        events.push(
            tokio::time::timeout(Duration::from_secs(3), events_rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tokio::time::timeout(Duration::from_secs(3), fake.wait_completed(3))
        .await
        .unwrap();
    shutdown_tx.send(true).unwrap();
    run.await.unwrap().unwrap();
    assert_eq!(*store.lock().unwrap(), Some(10));

    let hits = fake.hits().await;
    assert_eq!(hits[0].method(), "getMe");
    assert_json(&hits[1], "getUpdates");
    let first = json_body(&hits[1]);
    assert!(first.get("offset").is_none());
    assert_eq!(first["timeout"], LONG_POLL_TIMEOUT_SECS);
    assert_eq!(
        first["allowed_updates"],
        json!(["message", "message_reaction"])
    );
    let second = json_body(&hits[2]);
    assert_eq!(second["offset"], 10);

    match &events[0] {
        TopicIncoming::Message(message) => {
            assert_eq!(message.topic, TopicId(9));
            assert_eq!(message.message, TgMessageId(30));
            assert_eq!(message.sender_id, 7);
            assert_eq!(message.sender_display_name, "An Be");
            assert_eq!(message.text, "hello");
            assert_eq!(message.reply_to, Some(TgMessageId(3)));
            assert!(message.attachments.is_empty());
            assert_eq!(message.sent_at_unix_secs, 1_700_000_001);
        }
        TopicIncoming::Reaction(_) => panic!("text update"),
    }
    match &events[1] {
        TopicIncoming::Message(message) => {
            assert_eq!(message.topic, TopicId(limits::GENERAL_TOPIC_ID));
            assert_eq!(message.text, "general");
        }
        TopicIncoming::Reaction(_) => panic!("general update"),
    }
    match &events[2] {
        TopicIncoming::Message(message) => {
            assert_eq!(message.text, "pic");
            assert_eq!(message.media_group_id.as_deref(), Some("album-1"));
            assert_eq!(message.attachments, vec![IncomingMessage::photo_fixture()]);
        }
        TopicIncoming::Reaction(_) => panic!("photo update"),
    }
    match &events[3] {
        TopicIncoming::Message(message) => {
            assert_eq!(message.attachments.len(), 1);
            let voice = &message.attachments[0];
            assert_eq!(voice.kind, AttachmentKind::Voice);
            assert_eq!(voice.file_id, "v1");
            assert_eq!(voice.file_name.as_deref(), Some("voice.ogg"));
            assert_eq!(voice.mime.as_deref(), Some("audio/ogg"));
            assert_eq!(voice.duration_secs, Some(4));
            assert_eq!(voice.size_bytes, Some(40));
        }
        TopicIncoming::Reaction(_) => panic!("voice update"),
    }
    assert_eq!(
        events[4],
        TopicIncoming::Reaction(IncomingReaction {
            topic: None,
            message: TgMessageId(30),
            user_id: 7,
            emoji: "👍".into(),
            added: true,
        })
    );
    assert_eq!(
        events[5],
        TopicIncoming::Reaction(IncomingReaction {
            topic: None,
            message: TgMessageId(30),
            user_id: 7,
            emoji: "👎".into(),
            added: false,
        })
    );
}

impl IncomingMessage {
    fn photo_fixture() -> crate::IncomingAttachment {
        crate::IncomingAttachment {
            kind: AttachmentKind::Photo,
            file_id: "large".into(),
            file_name: Some("photo.jpg".into()),
            mime: Some("image/jpeg".into()),
            size_bytes: Some(1000),
            duration_secs: None,
        }
    }
}

struct MemOffsets {
    value: Arc<StdMutex<Option<i64>>>,
}

impl OffsetStore for MemOffsets {
    async fn load(&self) -> crate::Result<Option<i64>> {
        Ok(*self.value.lock().unwrap())
    }

    async fn save(&self, offset: i64) -> crate::Result<()> {
        *self.value.lock().unwrap() = Some(offset);
        Ok(())
    }
}

fn sample_updates() -> String {
    let group = format!(r#"{{"id":{GROUP_ID},"type":"supergroup"}}"#);
    let person = r#"{"id":7,"is_bot":false,"first_name":"An","last_name":"Be"}"#;
    format!(
        r#"{{"ok":true,"result":[
            {{"update_id":1,"message":{{"message_id":1,"from":{person},"date":1700000000,"chat":{{"id":-100999,"type":"supergroup"}},"text":"foreign"}}}},
            {{"update_id":2,"message":{{"message_id":2,"from":{{"id":42,"is_bot":true,"first_name":"Hub"}},"date":1700000000,"chat":{group},"text":"mine"}}}},
            {{"update_id":3,"message":{{"message_id":30,"message_thread_id":9,"from":{person},"date":1700000001,"chat":{group},"text":"hello","reply_to_message":{{"message_id":3,"date":1700000000,"chat":{group}}}}}}},
            {{"update_id":4,"message":{{"message_id":31,"from":{person},"date":1700000001,"chat":{group},"text":"general"}}}},
            {{"update_id":5,"message":{{"message_id":32,"message_thread_id":9,"from":{person},"date":1700000002,"chat":{group},"caption":"pic","media_group_id":"album-1","photo":[{{"file_id":"small","file_unique_id":"s","width":10,"height":10,"file_size":10}},{{"file_id":"large","file_unique_id":"l","width":100,"height":100,"file_size":1000}}]}}}},
            {{"update_id":6,"message":{{"message_id":33,"message_thread_id":9,"from":{person},"date":1700000003,"chat":{group},"voice":{{"file_id":"v1","file_unique_id":"vu","duration":4,"mime_type":"audio/ogg","file_size":40}}}}}},
            {{"update_id":7,"message_reaction":{{"chat":{{"id":-100999,"type":"supergroup"}},"message_id":1,"user":{person},"date":1700000000,"old_reaction":[],"new_reaction":[{{"type":"emoji","emoji":"👍"}}]}}}},
            {{"update_id":8,"message_reaction":{{"chat":{group},"message_id":30,"user":{person},"date":1700000004,"old_reaction":[{{"type":"emoji","emoji":"👎"}},{{"type":"custom_emoji","custom_emoji_id":"c1"}}],"new_reaction":[{{"type":"emoji","emoji":"👍"}},{{"type":"paid"}}]}}}},
            {{"update_id":9,"message_reaction":{{"chat":{group},"message_id":30,"user":{{"id":42,"is_bot":true,"first_name":"Hub"}},"date":1700000005,"old_reaction":[],"new_reaction":[{{"type":"emoji","emoji":"👀"}}]}}}}
        ]}}"#
    )
}

#[tokio::test]
async fn preflight_verdicts() {
    let ready = run_preflight(
        &chat_json("supergroup", true, None),
        &admin_json(true, true),
    )
    .await;
    assert_eq!(ready.bot_id, i64::try_from(BOT_ID).unwrap());
    assert_eq!(ready.bot_username.as_deref(), Some("hub_bot"));
    assert!(ready.is_supergroup && ready.is_forum);
    assert!(ready.can_manage_topics && ready.can_send_messages);
    assert!(ready.can_delete_messages && ready.can_react);
    assert_eq!(ready.verdict, PreflightVerdict::Ready);

    let group = run_preflight(&chat_json("group", false, None), &admin_json(true, true)).await;
    assert_eq!(group.verdict, PreflightVerdict::NotSupergroup);

    let forum = run_preflight(
        &chat_json("supergroup", false, None),
        &admin_json(true, true),
    )
    .await;
    assert_eq!(forum.verdict, PreflightVerdict::ForumDisabled);
    assert!(forum.is_supergroup);

    let topics = run_preflight(
        &chat_json("supergroup", true, None),
        &admin_json(false, true),
    )
    .await;
    assert_eq!(
        topics.verdict,
        PreflightVerdict::MissingRights(vec![BotRight::ManageTopics])
    );

    let delete = run_preflight(
        &chat_json("supergroup", true, None),
        &admin_json(true, false),
    )
    .await;
    assert_eq!(
        delete.verdict,
        PreflightVerdict::MissingRights(vec![BotRight::DeleteMessages])
    );

    let reactions = run_preflight(
        &chat_json("supergroup", true, Some("[]")),
        &admin_json(true, true),
    )
    .await;
    assert!(!reactions.can_react);
    assert_eq!(
        reactions.verdict,
        PreflightVerdict::MissingRights(vec![BotRight::React])
    );

    let member = run_preflight(
        &chat_json("supergroup", true, None),
        &format!(r#"{{"status":"member","user":{BOT_USER}}}"#),
    )
    .await;
    assert!(member.can_send_messages && member.can_react);
    assert_eq!(
        member.verdict,
        PreflightVerdict::MissingRights(vec![BotRight::ManageTopics, BotRight::DeleteMessages])
    );

    let left = run_preflight(
        &chat_json("supergroup", true, None),
        &format!(r#"{{"status":"left","user":{BOT_USER}}}"#),
    )
    .await;
    assert_eq!(
        left.verdict,
        PreflightVerdict::MissingRights(vec![
            BotRight::ManageTopics,
            BotRight::SendMessages,
            BotRight::DeleteMessages,
            BotRight::React,
        ])
    );
}

const BOT_USER: &str = r#"{"id":42,"is_bot":true,"first_name":"Hub","username":"hub_bot"}"#;

fn chat_json(kind: &str, forum: bool, reactions: Option<&str>) -> String {
    let reactions = match reactions {
        Some(value) => format!(r#","available_reactions":{value}"#),
        None => String::new(),
    };
    format!(
        r#"{{"id":{GROUP_ID},"type":"{kind}","is_forum":{forum}{reactions},"accepted_gift_types":{{"unlimited_gifts":false,"limited_gifts":false,"unique_gifts":false,"premium_subscription":false,"gifts_from_channels":false}}}}"#
    )
}

fn admin_json(manage_topics: bool, delete: bool) -> String {
    format!(
        r#"{{"status":"administrator","user":{BOT_USER},"can_be_edited":false,"is_anonymous":false,"can_manage_chat":true,"can_delete_messages":{delete},"can_manage_video_chats":false,"can_restrict_members":false,"can_promote_members":false,"can_change_info":false,"can_invite_users":true,"can_send_welcome_messages":false,"can_manage_topics":{manage_topics}}}"#
    )
}

async fn run_preflight(chat: &str, member: &str) -> PreflightReport {
    let me = envelope::<User>(BOT_USER);
    let chat_body = envelope::<ChatFullInfo>(chat);
    let member_body = envelope::<ChatMember>(member);
    let fake = Fake::bind(move |hit| match hit.method() {
        "getMe" => Reply::json(200, me.clone()),
        "getChat" => Reply::json(200, chat_body.clone()),
        "getChatMember" => Reply::json(200, member_body.clone()),
        _ => Reply::json(500, api_error(500, "unexpected", None)),
    })
    .await;
    let report = test_client(&fake.base).preflight().await.unwrap();
    let hits = fake.hits().await;
    assert_eq!(hits[0].method(), "getMe");
    assert_json(&hits[1], "getChat");
    assert_eq!(json_body(&hits[1])["chat_id"], GROUP_ID);
    assert_json(&hits[2], "getChatMember");
    assert_eq!(json_body(&hits[2])["user_id"], BOT_ID);
    report
}

#[tokio::test]
async fn token_never_appears_in_debug_or_errors() {
    let token = BotToken::new(TOKEN);
    assert_eq!(format!("{token:?}"), "<redacted>");
    assert_eq!(token.to_string(), "<redacted>");
    let config = BotConfig {
        token: token.clone(),
        api_base_url: "http://127.0.0.1:1".into(),
        group_id: GROUP_ID,
    };
    assert!(!format!("{config:?}").contains(TOKEN));
    let client = BotClient::new(config).unwrap();
    assert!(!format!("{client:?}").contains(TOKEN));
    let err = client
        .send_text(TopicId(THREAD), "x".into(), None)
        .await
        .unwrap_err();
    let rendered = format!("{err} {err:?} {}", err.adapter_detail());
    assert!(!rendered.contains(TOKEN));
    assert!(matches!(err, Error::Transient { status: None }));

    let leaked = api_error(400, &format!("Bad Request: token {TOKEN}"), None);
    let fake = Fake::bind(move |_| Reply::json(400, leaked.clone())).await;
    let err = test_client(&fake.base)
        .send_text(TopicId(THREAD), "x".into(), None)
        .await
        .unwrap_err();
    let rendered = format!("{err} {err:?} {}", err.adapter_detail());
    assert!(!rendered.contains(TOKEN));
    assert_eq!(err, Error::Permanent { status: Some(400) });

    let rejected = BotClient::new(BotConfig {
        token: BotToken::new(TOKEN),
        api_base_url: format!("http://127.0.0.1/{TOKEN}"),
        group_id: GROUP_ID,
    })
    .unwrap_err();
    assert!(!format!("{rejected} {rejected:?}").contains(TOKEN));
    assert!(matches!(rejected, Error::Permanent { status: None }));
}
