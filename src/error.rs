use crate::fcm_sender::FcmError;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use redis::RedisError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ServiceError {
    #[error("Redis error: {0}")]
    Redis(#[from] RedisError),

    #[error("FCM error: {0}")]
    Fcm(#[from] FcmError),

    #[error("Nostr SDK error: {0}")]
    NostrSdk(#[from] nostr_sdk::client::Error),

    #[error("Nostr Event error: {0}")]
    NostrKeys(#[from] nostr_sdk::key::Error),

    #[error("Tokio task join error: {0}")]
    TokioJoin(#[from] tokio::task::JoinError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("Serde JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Operation cancelled")]
    Cancelled,
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let (status, error_message) = match self {
            ServiceError::Config(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Configuration error: {}", e),
            ),
            ServiceError::Redis(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Redis error: {}", e),
            ),
            ServiceError::NostrSdk(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Nostr SDK error: {}", e),
            ),
            ServiceError::NostrKeys(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Nostr event error: {}", e),
            ),
            ServiceError::TokioJoin(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Task join error: {}", e),
            ),
            ServiceError::Io(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("IO error: {}", e),
            ),
            ServiceError::SerdeJson(e) => (StatusCode::BAD_REQUEST, format!("JSON error: {}", e)),
            ServiceError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            ServiceError::Fcm(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("FCM error: {}", e),
            ),
            ServiceError::Cancelled => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Operation cancelled".to_string(),
            ),
        };

        let body = Json(serde_json::json!({ "error": error_message }));
        (status, body).into_response()
    }
}

pub type Result<T, E = ServiceError> = std::result::Result<T, E>;
