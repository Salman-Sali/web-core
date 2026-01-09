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

impl serde::Serialize for SomethingWentWrong {
    fn serialize<__S>(&self, __serializer: __S) -> serde::__private228::Result<__S::Ok, __S::Error>
    where
        __S: serde::Serializer,
    {
        let mut _serde_state = serde::Serializer::serialize_struct(
            __serializer,
            "SomethingWentWrong",
            false as usize + 1 + 1,
        )?;
        serde::ser::SerializeStruct::serialize_field(
            &mut _serde_state,
            "error",
            "Something went wrong",
        )?;
        serde::ser::SerializeStruct::serialize_field(
            &mut _serde_state,
            "error_id",
            &self.error_id,
        )?;

        #[cfg(any(feature = "local", feature = "test_mode"))]
        {
            serde::ser::SerializeStruct::serialize_field(
                &mut _serde_state,
                "error_details",
                &self.error_details,
            )?;
        }
        serde::ser::SerializeStruct::end(_serde_state)
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
