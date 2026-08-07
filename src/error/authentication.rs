use axum::{Json, response::IntoResponse};
use http::StatusCode;
use serde::ser::SerializeStruct;

#[derive(Debug, serde::Deserialize)]
pub struct AuthenticationError {
    pub error_details: String,
}

impl serde::Serialize for AuthenticationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let field_count = if cfg!(debug_assertions) { 2 } else { 1 };
        let mut state = serializer.serialize_struct("AuthenticationError", field_count)?;
        state.serialize_field("error", "Unauthorized")?;
        if cfg!(debug_assertions) {
            state.serialize_field("error_details", &self.error_details)?;
        }
        state.end()
    }
}

impl IntoResponse for AuthenticationError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::UNAUTHORIZED, Json(serde_json::json!(self))).into_response()
    }
}

impl AuthenticationError {
    pub fn new(error_details: impl std::fmt::Display) -> Self {
        Self {
            error_details: error_details.to_string(),
        }
    }
}
