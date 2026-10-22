//! Failures from the Bot API adapter.
//!
//! [`Error::RateLimited`] and [`Error::TopicGone`] match the variants
//! [`hub_core::Error`] uses. The other variants stay distinct on [`crate::BotClient`]
//! methods. [`TopicSink`](hub_core::TopicSink) has no slot for them, so
//! [`From<Error>`] for [`hub_core::Error`] folds them into [`hub_core::Error::Adapter`]
//! with a stable `detail` prefix.

use crate::limits::{
    HTTP_FORBIDDEN, HTTP_REQUEST_TIMEOUT, HTTP_SERVER_ERROR_MAX, HTTP_SERVER_ERROR_MIN,
    HTTP_TOO_MANY_REQUESTS,
};

/// Which size cap an attachment crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// The bridge would have to download the file.
    Download,
    /// The bridge would upload the file as multipart.
    Upload,
}

impl std::fmt::Display for TransferDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Download => "download",
            Self::Upload => "upload",
        })
    }
}

/// Failure from a [`crate::BotClient`] call.
///
/// `Display` and `Debug` carry a status or a size. They do not carry the bot token,
/// message text, or the raw `Telegram` description.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// `Telegram` asked the caller to wait.
    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds `Telegram` asked the caller to wait.
        retry_after_secs: u64,
    },
    /// The forum topic was deleted or closed.
    #[error("topic is gone")]
    TopicGone,
    /// The bot is missing a right the call needs.
    #[error("bot lacks rights")]
    LacksRights,
    /// The attachment is over the download or upload cap.
    ///
    /// The bridge can send a text note instead.
    #[error("attachment of {size_bytes} bytes exceeds the {limit_bytes} byte {direction} limit")]
    TooLarge {
        /// Which cap was crossed.
        direction: TransferDirection,
        /// Size the caller reported, or the file length on disk.
        size_bytes: u64,
        /// The cap, in bytes.
        limit_bytes: u64,
    },
    /// A timeout, a connection failure, or an HTTP 5xx. Retrying the same call may work.
    ///
    /// `status` is present when the response had an HTTP status. `Display` names the class.
    /// [`Error::adapter_detail`] includes the status.
    #[error("transient telegram failure")]
    Transient {
        /// HTTP status, when the response had one.
        status: Option<u16>,
    },
    /// The call will fail again until the request or the bot setup changes.
    ///
    /// `status` is present when the response had an HTTP status.
    #[error("permanent telegram failure")]
    Permanent {
        /// HTTP status, when the response had one.
        status: Option<u16>,
    },
}

/// Result of a fallible [`crate::BotClient`] call.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Log-safe text for [`hub_core::Error::Adapter`].
    ///
    /// The string starts with a stable word: `too_large`, `bot lacks rights`,
    /// `transient`, or `permanent`.
    pub fn adapter_detail(&self) -> String {
        match self {
            Self::RateLimited { retry_after_secs } => {
                format!("rate limited, retry after {retry_after_secs}s")
            }
            Self::TopicGone => "topic is gone".to_owned(),
            Self::LacksRights => "bot lacks rights".to_owned(),
            Self::TooLarge {
                direction,
                size_bytes,
                limit_bytes,
            } => format!("too_large {direction} {size_bytes} bytes over {limit_bytes}"),
            Self::Transient { status } => match status {
                Some(code) => format!("transient status {code}"),
                None => "transient".to_owned(),
            },
            Self::Permanent { status } => match status {
                Some(code) => format!("permanent status {code}"),
                None => "permanent".to_owned(),
            },
        }
    }
}

impl From<Error> for hub_core::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::RateLimited { retry_after_secs } => {
                hub_core::Error::RateLimited { retry_after_secs }
            }
            Error::TopicGone => hub_core::Error::TopicGone,
            other => hub_core::Error::Adapter {
                detail: other.adapter_detail(),
            },
        }
    }
}

/// Maps a Bot API error object onto [`Error`].
///
/// `description` is matched and then dropped. It is not stored on the error.
pub(crate) fn classify_api(error_code: u64, description: &str, retry_after: Option<u64>) -> Error {
    if error_code == HTTP_TOO_MANY_REQUESTS {
        return Error::RateLimited {
            retry_after_secs: retry_after.unwrap_or(crate::limits::DEFAULT_RETRY_AFTER_SECS),
        };
    }
    let description = description.to_ascii_lowercase();
    if is_topic_gone(&description) {
        return Error::TopicGone;
    }
    if error_code == HTTP_FORBIDDEN || lacks_rights(&description) {
        return Error::LacksRights;
    }
    let status = u16::try_from(error_code).ok();
    if error_code == HTTP_REQUEST_TIMEOUT
        || (HTTP_SERVER_ERROR_MIN..=HTTP_SERVER_ERROR_MAX).contains(&error_code)
    {
        return Error::Transient { status };
    }
    Error::Permanent { status }
}

fn is_topic_gone(description: &str) -> bool {
    description.contains("message thread not found")
        || description.contains("topic_deleted")
        || description.contains("topic_closed")
}

fn lacks_rights(description: &str) -> bool {
    description.contains("not enough rights")
        || description.contains("have no rights")
        || description.contains("chat_admin_required")
        || description.contains("need administrator")
}

pub(crate) fn map_frankenstein(err: frankenstein::Error) -> Error {
    match err {
        frankenstein::Error::Api(body) => {
            let retry_after = body
                .parameters
                .as_ref()
                .and_then(|params| params.retry_after)
                .map(u64::from);
            classify_api(body.error_code, &body.description, retry_after)
        }
        frankenstein::Error::HttpReqwest(http) => {
            if let Some(status) = http.status() {
                classify_api(u64::from(status.as_u16()), "", None)
            } else {
                Error::Transient { status: None }
            }
        }
        frankenstein::Error::JsonDecode { .. } | frankenstein::Error::JsonEncode { .. } => {
            Error::Permanent { status: None }
        }
        frankenstein::Error::ReadFile(_) => Error::Permanent { status: None },
        _ => Error::Transient { status: None },
    }
}

/// `Some` when this failure is a 429 the queue should wait out.
pub(crate) fn retry_after_secs(err: &frankenstein::Error) -> Option<u64> {
    let frankenstein::Error::Api(body) = err else {
        return None;
    };
    if body.error_code != HTTP_TOO_MANY_REQUESTS {
        return None;
    }
    Some(
        body.parameters
            .as_ref()
            .and_then(|params| params.retry_after)
            .map(u64::from)
            .unwrap_or(crate::limits::DEFAULT_RETRY_AFTER_SECS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_table_matches_telegram_wording() {
        let cases = [
            (
                429,
                "Too Many Requests: retry after 5",
                Some(5),
                Error::RateLimited {
                    retry_after_secs: 5,
                },
            ),
            (
                429,
                "Too Many Requests",
                None,
                Error::RateLimited {
                    retry_after_secs: 30,
                },
            ),
            (
                400,
                "Bad Request: message thread not found",
                None,
                Error::TopicGone,
            ),
            (400, "Bad Request: TOPIC_DELETED", None, Error::TopicGone),
            (400, "Bad Request: TOPIC_CLOSED", None, Error::TopicGone),
            (
                400,
                "Bad Request: not enough rights to manage topics",
                None,
                Error::LacksRights,
            ),
            (
                400,
                "Bad Request: have no rights to send a message",
                None,
                Error::LacksRights,
            ),
            (
                400,
                "Bad Request: CHAT_ADMIN_REQUIRED",
                None,
                Error::LacksRights,
            ),
            (
                400,
                "Bad Request: need administrator rights in the chat",
                None,
                Error::LacksRights,
            ),
            (
                403,
                "Forbidden: bot was blocked by the user",
                None,
                Error::LacksRights,
            ),
            (
                500,
                "Internal Server Error",
                None,
                Error::Transient { status: Some(500) },
            ),
            (
                408,
                "Request Timeout",
                None,
                Error::Transient { status: Some(408) },
            ),
            (
                400,
                "Bad Request: message text is empty",
                None,
                Error::Permanent { status: Some(400) },
            ),
            (
                401,
                "Unauthorized",
                None,
                Error::Permanent { status: Some(401) },
            ),
        ];
        for (code, description, retry_after, expected) in cases {
            assert_eq!(
                classify_api(code, description, retry_after),
                expected,
                "{description}"
            );
        }
    }

    #[test]
    fn display_and_adapter_detail_omit_the_description() {
        let err = classify_api(
            400,
            "Bad Request: message thread not found token 123:test",
            None,
        );
        let rendered = format!("{err} {err:?} {}", err.adapter_detail());
        assert!(!rendered.contains("123:test"));
        assert!(!rendered.contains("message thread"));
        assert!(matches!(err, Error::TopicGone));
    }

    #[test]
    fn hub_core_keeps_rate_limit_and_topic_gone() {
        let limited = hub_core::Error::from(Error::RateLimited {
            retry_after_secs: 4,
        });
        assert_eq!(
            limited,
            hub_core::Error::RateLimited {
                retry_after_secs: 4
            }
        );
        assert_eq!(
            hub_core::Error::from(Error::TopicGone),
            hub_core::Error::TopicGone
        );
        let folded = hub_core::Error::from(Error::TooLarge {
            direction: TransferDirection::Upload,
            size_bytes: 60,
            limit_bytes: 50,
        });
        assert!(matches!(
            folded,
            hub_core::Error::Adapter { detail } if detail.starts_with("too_large upload")
        ));
    }
}
