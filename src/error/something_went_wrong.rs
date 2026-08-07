use axum::{
    Json,
    response::{IntoResponse, Response},
};
use http::StatusCode;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SomethingWentWrong {
    pub error_id: String,

    pub error_details: String,
}

use serde::ser::SerializeStruct;
use std::result::Result;

impl serde::Serialize for SomethingWentWrong {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let field_count = if cfg!(debug_assertions) { 3 } else { 2 };
        let mut state = serializer.serialize_struct("SomethingWentWrong", field_count)?;

        state.serialize_field("error", "Something went wrong")?;
        state.serialize_field("error_id", &self.error_id)?;
        if cfg!(debug_assertions) {
            state.serialize_field("error_details", &self.error_details)?;
        }

        state.end()
    }
}

impl SomethingWentWrong {
    pub fn print_error(&self) {
        eprintln!("Error: {}", self.error_id);
        eprintln!("Something went wrong : {}", self.error_details);
    }
}

impl SomethingWentWrong {
    pub fn new(error_details: impl std::fmt::Debug) -> Self {
        Self {
            error_id: format!("Error-{}", uuid::Uuid::new_v4()),
            error_details: format!("{:?}", error_details),
        }
    }
}

impl IntoResponse for SomethingWentWrong {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!(self)),
        )
            .into_response()
    }
}
