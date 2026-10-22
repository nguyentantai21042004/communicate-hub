//! Turns protocol errors into metric events, so API pressure shows up in the report.

use captain_store::{MetricEvent, MetricKind};

use crate::app::Db;

const OS_TIMED_OUT: &str = "os error 60";
const OS_UNREACHABLE: &str = "os error 51";
const OS_RESET: &str = "os error 54";
const NO_OPEN_SENDER: &str = "no open home sender"; // keepalive refused to open a connection

/// Why an update stream was dropped. Stored as the metric tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropClass {
    /// Read returned `ETIMEDOUT` (`os error 60`).
    Timeout,
    /// `ENETUNREACH` (`os error 51`).
    NetworkUnreachable,
    /// The server or the local sender closed the stream.
    ServerClosed,
    /// A failure that is not one of the classes above.
    Other,
}

impl DropClass {
    /// Tag stored on the metric event and shown in status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::NetworkUnreachable => "network_unreachable",
            Self::ServerClosed => "server_closed",
            Self::Other => "other",
        }
    }

    /// Network and stream failures that need a new connection. Other RPC errors do not.
    pub fn is_link_failure(self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// One dropped update stream, written once the reconnect attempt has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDrop {
    /// When the drop was noticed, Unix seconds.
    pub at: i64,
    /// Seconds from the drop until the stream was back.
    pub secs: i64,
    /// Why it dropped.
    pub class: DropClass,
    /// Whether catch-up after the reconnect closed the gap.
    pub gap_closed: bool,
}

/// Classifies a protocol error from the text grammers leaves in it.
pub fn classify_error(error: &captain_protocol::Error) -> DropClass {
    classify_text(&error.to_string())
}

fn classify_text(message: &str) -> DropClass {
    if message.contains(OS_TIMED_OUT) || message.contains("timed out") {
        DropClass::Timeout
    } else if message.contains(OS_UNREACHABLE) || message.contains("Network is unreachable") {
        DropClass::NetworkUnreachable
    } else if message.contains(OS_RESET)
        || message.contains("Connection reset")
        || message.contains("dropped")
        || message.contains(NO_OPEN_SENDER)
    {
        DropClass::ServerClosed
    } else {
        DropClass::Other
    }
}

fn stream_drop_event(drop: &StreamDrop) -> MetricEvent {
    MetricEvent {
        a: Some(drop.secs),
        b: Some(if drop.gap_closed { 1 } else { 0 }),
        tag: Some(drop.class.as_str().to_owned()),
        ..MetricEvent::new(MetricKind::StreamDrop, drop.at)
    }
}

/// Records a dropped update stream. Never fails: a metric must not break the caller.
pub async fn record_drop(db: &Db, drop: StreamDrop) {
    let event = stream_drop_event(&drop);
    if let Err(e) = db.run(move |s| s.record(&event)).await {
        tracing::warn!(error = %e, "could not record a stream drop");
    }
}

/// Records a failed Telegram request. Never fails: a metric must not break the caller.
pub async fn record_error(db: &Db, error: &captain_protocol::Error, now: i64) {
    let captain_protocol::Error::Rpc { request, message } = error else {
        return;
    };
    let event = match flood_wait_secs(message) {
        Some(secs) => MetricEvent {
            a: Some(secs),
            tag: Some((*request).to_owned()),
            ..MetricEvent::new(MetricKind::FloodWait, now)
        },
        None => MetricEvent {
            tag: Some(match error_name(message) {
                Some(name) => format!("{request} {name}"),
                None => (*request).to_owned(),
            }),
            ..MetricEvent::new(MetricKind::RpcError, now)
        },
    };
    if let Err(e) = db.run(move |s| s.record(&event)).await {
        tracing::warn!(error = %e, "could not record an API error");
    }
}

/// TELEGRAM: grammers 0.10 prints `FLOOD_WAIT (value: X)`
/// (<https://core.telegram.org/api/errors#420-flood>).
fn flood_wait_secs(message: &str) -> Option<i64> {
    match captain_protocol::classify_failure(message) {
        captain_protocol::TelegramFailure::FloodWait(secs) => i64::try_from(secs).ok(),
        captain_protocol::TelegramFailure::NotAuthorized
        | captain_protocol::TelegramFailure::Other => None,
    }
}

/// The first all-caps token, such as `PEER_ID_INVALID`.
fn error_name(message: &str) -> Option<&str> {
    message
        .split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .find(|t| t.len() > 2 && t.starts_with(|c: char| c.is_ascii_uppercase()) && t.contains('_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flood_waits_and_error_names() {
        assert_eq!(
            flood_wait_secs("rpc error 420: FLOOD_WAIT (value: 31)"),
            Some(31)
        );
        assert_eq!(
            flood_wait_secs("rpc error 420: SLOWMODE_WAIT (value: 12)"),
            Some(12)
        );
        assert_eq!(flood_wait_secs("PEER_ID_INVALID"), None);
        assert_eq!(
            error_name("rpc error 400: PEER_ID_INVALID caused by"),
            Some("PEER_ID_INVALID")
        );
        assert_eq!(error_name("network down"), None);
    }

    #[test]
    fn classifies_the_failures_from_the_daemon_log() {
        use captain_protocol::Error;

        let timeout = Error::Rpc {
            request: "updates.getDifference",
            message: "request error: Operation timed out (os error 60)".into(),
        };
        let down = Error::Rpc {
            request: "users.getUsers",
            message: "request error: Network is unreachable (os error 51)".into(),
        };
        let reset = Error::Rpc {
            request: "updates.getDifference",
            message: "request error: Connection reset by peer (os error 54)".into(),
        };
        let flood = Error::Rpc {
            request: "messages.getHistory",
            message: "rpc error 420: FLOOD_WAIT (value: 31)".into(),
        };
        assert_eq!(classify_error(&timeout), DropClass::Timeout);
        assert_eq!(classify_error(&down), DropClass::NetworkUnreachable);
        assert_eq!(classify_error(&reset), DropClass::ServerClosed);
        let closed = Error::Rpc {
            request: "ping",
            message: "no open home sender".into(),
        };
        assert_eq!(classify_error(&flood), DropClass::Other);
        assert_eq!(classify_error(&closed), DropClass::ServerClosed);
        assert!(DropClass::Timeout.is_link_failure());
        assert!(DropClass::ServerClosed.is_link_failure());
        assert!(!DropClass::Other.is_link_failure());
    }

    #[test]
    fn stream_drop_event_keeps_duration_class_and_gap() {
        let event = stream_drop_event(&StreamDrop {
            at: 10,
            secs: 4,
            class: DropClass::Timeout,
            gap_closed: false,
        });
        assert_eq!(event.kind, MetricKind::StreamDrop);
        assert_eq!(event.at, 10);
        assert_eq!(event.a, Some(4));
        assert_eq!(event.b, Some(0));
        assert_eq!(event.tag.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn record_drop_is_readable_from_an_old_schema() {
        use captain_store::Store;

        let db = Db::new(Store::open_in_memory().unwrap());
        record_drop(
            &db,
            StreamDrop {
                at: 50,
                secs: 3,
                class: DropClass::ServerClosed,
                gap_closed: true,
            },
        )
        .await;
        let count = db
            .run(|s| s.count_metrics(MetricKind::StreamDrop, 0))
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}
