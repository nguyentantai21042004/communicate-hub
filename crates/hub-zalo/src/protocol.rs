//! `NDJSON` frames shared with `sidecars/zalo-node`.
//!
//! Method, event, and error names are the lists in `docs/design/zalo-sidecar.md`.

use serde_json::Value;

use crate::error::{Error, Result};

/// Method names, in the order the design doc lists them.
pub const METHOD_NAMES: &[&str] = &[
    "health",
    "login.qr.start",
    "login.qr.cancel",
    "send.text",
    "send.media",
    "send.reaction",
    "send.unsend",
    "directory.friends",
    "directory.groups",
    "directory.group_members",
    "friend.requests",
    "friend.accept",
    "friend.send_request",
    "group.join",
    "group.leave",
];

/// Event names, in the order the design doc lists them.
pub const EVENT_NAMES: &[&str] = &[
    "message",
    "reaction",
    "undo",
    "group_event",
    "friend_request",
    "session",
    "directory",
    "listener",
    "login.qr.state",
];

/// Closed error codes, in the order the design doc lists them.
pub const ERROR_CODES: &[&str] = &[
    "auth_expired",
    "rate_limited",
    "not_found",
    "unsupported",
    "timeout",
    "internal",
];

pub(crate) const SESSION_VALID: &str = "valid";
pub(crate) const SESSION_EXPIRED: &str = "expired";
pub(crate) const SESSION_DISCONNECTED: &str = "disconnected";
pub(crate) const SESSION_RECONNECTING: &str = "reconnecting";

/// One stdout object that is either a response or an event.
#[derive(Debug)]
pub(crate) enum StdoutFrame {
    Response {
        id: u64,
        result: Value,
    },
    Failure {
        id: u64,
        code: String,
        retry_after_secs: Option<u64>,
    },
    Event {
        name: String,
        data: Value,
    },
    Ignore,
}

/// One event the supervisor broadcasts to the adapter and the admin handle.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarEvent {
    /// Contract event name.
    pub name: String,
    /// Event `data` object.
    pub data: Value,
}

pub(crate) fn parse_stdout(line: &str) -> StdoutFrame {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return StdoutFrame::Ignore,
    };
    let Some(obj) = value.as_object() else {
        return StdoutFrame::Ignore;
    };
    if let (Some(id), Some(ok)) = (
        u64_field(obj.get("id")),
        obj.get("ok").and_then(Value::as_bool),
    ) {
        if ok {
            return StdoutFrame::Response {
                id,
                result: obj.get("result").cloned().unwrap_or(Value::Null),
            };
        }
        let error = obj.get("error");
        let code = error
            .and_then(|item| item.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("internal");
        let retry_after_secs = error
            .and_then(|item| item.get("retry_after_secs"))
            .and_then(Value::as_u64);
        return StdoutFrame::Failure {
            id,
            code: code.to_owned(),
            retry_after_secs,
        };
    }
    if let Some(name) = obj.get("event").and_then(Value::as_str) {
        return StdoutFrame::Event {
            name: name.to_owned(),
            data: obj.get("data").cloned().unwrap_or(Value::Null),
        };
    }
    StdoutFrame::Ignore
}

pub(crate) fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn u64_field(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

pub(crate) fn map_rpc(err: Error) -> hub_core::Error {
    match err {
        Error::Rpc {
            code,
            retry_after_secs,
        } if code == "rate_limited" => hub_core::Error::RateLimited {
            retry_after_secs: retry_after_secs
                .unwrap_or(crate::limits::DEFAULT_RATE_LIMIT_RETRY_SECS),
        },
        Error::Rpc { code, .. } => hub_core::Error::Adapter { detail: code },
        Error::Timeout => hub_core::Error::Adapter {
            detail: "timeout".into(),
        },
        Error::CrashLoop => hub_core::Error::Adapter {
            detail: "crash loop".into(),
        },
        Error::Protocol => hub_core::Error::Adapter {
            detail: "protocol".into(),
        },
        _ => hub_core::Error::Adapter {
            detail: "sidecar".into(),
        },
    }
}

pub(crate) fn message_id_of(result: &Value) -> Result<String> {
    string_field(result, "message_id").ok_or(Error::Protocol)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_the_design_doc() {
        let doc = include_str!("../../../docs/design/zalo-sidecar.md");
        assert_eq!(
            METHOD_NAMES,
            section(doc, "<!-- contract:methods -->").as_slice()
        );
        assert_eq!(
            EVENT_NAMES,
            section(doc, "<!-- contract:events -->").as_slice()
        );
        assert_eq!(
            ERROR_CODES,
            section(doc, "<!-- contract:errors -->").as_slice()
        );
    }

    fn section(doc: &str, marker: &str) -> Vec<String> {
        let after = doc.split_once(marker).unwrap().1;
        let fenced = after.split("```json").nth(1).unwrap();
        let body = fenced.split("```").next().unwrap();
        serde_json::from_str(body.trim()).unwrap()
    }
}
