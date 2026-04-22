//! HTTP-side error type for the `cqs serve` web UI.
//!
//! Wraps internal `cqs::store::StoreError` and other failure modes so they
//! can render as proper HTTP responses (5xx for internal failures, 4xx for
//! bad input, 404 for missing chunks). Implements `axum::response::IntoResponse`
//! so handler functions can `?`-propagate.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServeError {
    #[error("store error: {0}")]
    Store(#[from] crate::store::StoreError),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    detail: String,
}

impl IntoResponse for ServeError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ServeError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ServeError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ServeError::Store(_) | ServeError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        // Log internal failures so they reach the journal even when the
        // browser sees a generic 500. NotFound and BadRequest are user-facing
        // and don't warrant a warn-level log.
        match &self {
            ServeError::Store(e) => tracing::warn!(error = %e, "serve handler failed: store"),
            ServeError::Internal(e) => tracing::warn!(error = %e, "serve handler failed: internal"),
            _ => tracing::debug!(error = %self, "serve handler returned client error"),
        }
        let body = ErrorBody {
            error: code.to_string(),
            detail: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_is_404() {
        let err = ServeError::NotFound("chunk".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bad_request_is_400() {
        let err = ServeError::BadRequest("missing q".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn internal_is_500() {
        let err = ServeError::Internal("oops".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
