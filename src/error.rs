use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("upstream fetch failed: {0}")]
    Upstream(String),

    #[error("aggregation failed: {0}")]
    Aggregate(String),

    #[error("cache error: {0}")]
    Cache(String),

    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Upstream(m) => (StatusCode::BAD_GATEWAY, m.clone()),
            AppError::Aggregate(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            AppError::Cache(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
            AppError::Internal(e) => {
                (StatusCode::INTERNAL_SERVER_ERROR, format!("internal: {e}"))
            }
        };
        tracing::warn!(status = %status, error = %msg, "request failed");
        (status, Json(json!({ "error": msg }))).into_response()
    }
}
