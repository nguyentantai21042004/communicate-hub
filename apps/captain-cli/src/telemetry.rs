//! Turns protocol errors into metric events, so API pressure shows up in the report.

use captain_store::{MetricEvent, MetricKind};

use crate::app::Db;

const FLOOD_WAIT_PREFIX: &str = "FLOOD_WAIT_";

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

/// TELEGRAM: flood errors are named `FLOOD_WAIT_X`, X being the seconds to wait
/// (<https://core.telegram.org/api/errors#420-flood>).
fn flood_wait_secs(message: &str) -> Option<i64> {
    let start = message.find(FLOOD_WAIT_PREFIX)? + FLOOD_WAIT_PREFIX.len();
    let digits: String = message[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
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
            flood_wait_secs("rpc error 420: FLOOD_WAIT_31 caused by x"),
            Some(31)
        );
        assert_eq!(flood_wait_secs("PEER_ID_INVALID"), None);
        assert_eq!(
            error_name("rpc error 400: PEER_ID_INVALID caused by"),
            Some("PEER_ID_INVALID")
        );
        assert_eq!(error_name("network down"), None);
    }
}
