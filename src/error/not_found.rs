use axum::{response::IntoResponse, Json};
use http::StatusCode;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct NotFoundError {
    pub error: String
}

impl IntoResponse for NotFoundError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!(self))
        ).into_response()
    }
}

impl NotFoundError {
    pub fn new(error: String) -> Self {
        Self { error }
    }
}