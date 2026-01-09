use axum::{response::IntoResponse, Json};
use http::StatusCode;

#[derive(Debug, serde::Deserialize)]
pub struct AuthenticationError {
    pub error_details: String,
}

impl IntoResponse for AuthenticationError {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!(self))
        ).into_response()
    }
}

impl serde::Serialize for AuthenticationError {
        fn serialize<__S>(&self, __serializer: __S) -> serde::__private228::Result<__S::Ok, __S::Error>
        where
            __S: serde::Serializer,
        {
            let mut _serde_state = serde::Serializer::serialize_struct(__serializer, "AuthenticationError", false as usize + 1)?;
            serde::ser::SerializeStruct::serialize_field(&mut _serde_state, "error", "Unauthorized")?;
            #[cfg(any(feature = "local", feature = "test_mode"))]
            {
                serde::ser::SerializeStruct::serialize_field(&mut _serde_state, "error_details", &self.error_details)?;
            }
            serde::ser::SerializeStruct::end(_serde_state)
        }
    }

impl AuthenticationError {
    pub fn new(error_details: impl std::fmt::Display) -> Self {
        Self {
            error_details: error_details.to_string(),
        }
    }
}
