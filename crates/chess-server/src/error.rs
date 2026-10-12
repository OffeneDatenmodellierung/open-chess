//! API errors, in Lichess's shape: `{"error": "..."}` with a status code.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::warn;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> ApiError {
        ApiError { status, message: message.into() }
    }

    pub fn bad(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(what: &str) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, format!("No such {what}"))
    }

    pub fn unauthorized() -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "No such token")
    }

    pub fn forbidden(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::FORBIDDEN, message)
    }

    pub fn missing_scope(scopes: &[&str]) -> ApiError {
        ApiError::forbidden(format!("Missing scope: {}", scopes.join(" or ")))
    }

    pub fn too_many(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::TOO_MANY_REQUESTS, message)
    }

    /// The Board/Bot API's answer to a move or action that can't happen now.
    pub fn not_your_turn() -> ApiError {
        ApiError::bad("Not your turn, or game already over")
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        warn!("internal error: {e:#}");
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "temporarily unavailable, try again")
    }
}

impl From<tokio_postgres::Error> for ApiError {
    fn from(e: tokio_postgres::Error) -> Self {
        ApiError::from(anyhow::Error::from(e))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}
