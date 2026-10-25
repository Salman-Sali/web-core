use axum::{Json, response::IntoResponse};
use http::StatusCode;

/// 409 Conflict, serialised as `{"error": "<msg>"}`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConflictError {
    pub error: String,
}

impl IntoResponse for ConflictError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::CONFLICT, Json(serde_json::json!(self))).into_response()
    }
}

impl ConflictError {
    pub fn new(error: String) -> Self {
        Self { error }
    }
}
