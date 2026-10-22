//! Failures a dashboard route can turn into a JSON status.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Why a dashboard call did not return its body.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The query or body is not usable. The route answers 400.
    #[error("{0}")]
    BadRequest(String),
    /// The action has no implementation yet. The route answers 501.
    #[error("{0}")]
    NotImplemented(String),
    /// The store or the daemon could not answer. The route answers 400, matching the
    /// existing UI, which uses that status for every failed call.
    #[error("{0}")]
    Failed(String),
}

/// Result of one dashboard call.
pub type Result<T> = std::result::Result<T, Error>;

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self {
            Self::BadRequest(_) | Self::Failed(_) => StatusCode::BAD_REQUEST,
            Self::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
        };
        let body = Json(json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}
