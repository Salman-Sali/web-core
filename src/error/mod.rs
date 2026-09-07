pub mod authentication;
pub mod bad_request;
pub mod field_validation;
pub mod not_found;
pub mod something_went_wrong;

use std::{collections::HashMap, fmt::Debug};

use authentication::AuthenticationError;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bad_request::BadRequestError;
use field_validation::FieldValidationErrors;
use not_found::NotFoundError;
use something_went_wrong::SomethingWentWrong;
use web_core_validator::ValidationErrors;

use crate::error::field_validation::{FieldErrorKind, FieldErrors};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Error {
    FieldValidationError(FieldValidationErrors),
    BadRequestError(BadRequestError),
    SomethingWentWrong(SomethingWentWrong),
    AuthenticationFailure(AuthenticationError),
    NotFound(NotFoundError),
}

impl Error {
    ///Error info is not shown in api response. It is only printed to console.
    pub fn new_unauthorized(error_info: &str) -> Error {
        let authentication_error = AuthenticationError::new(error_info.to_string());
        Error::AuthenticationFailure(authentication_error)
    }

    pub fn new_field_validation_error(field: &str, error: &str) -> Error {
        let mut fields = HashMap::new();
        fields.insert(field.into(), FieldErrorKind::Message(error.into()));
        let field_validation_errors = FieldValidationErrors {
            fields: FieldErrors(fields),
        };
        Error::FieldValidationError(field_validation_errors)
    }

    ///Error info is not shown in api response. It is only printed to console.
    pub fn new_something_went_wrong(error_info: String) -> Error {
        let something_went_wrong = SomethingWentWrong::new(error_info);
        Error::SomethingWentWrong(something_went_wrong)
    }

    pub fn bad_request_error(message: &str) -> Error {
        Error::BadRequestError(BadRequestError::new(message.into()))
    }

    pub fn new_not_found(message: &str) -> Error {
        let not_found_error = NotFoundError::new(message.to_string());
        Error::NotFound(not_found_error)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Error::FieldValidationError(field_validation_errors) => {
                (StatusCode::BAD_REQUEST, Json(field_validation_errors)).into_response()
            }
            Error::BadRequestError(bad_request_error) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!(bad_request_error)),
            )
                .into_response(),
            Error::SomethingWentWrong(something_went_wrong) => {
                something_went_wrong.print_error();
                something_went_wrong.into_response()
            }
            Error::AuthenticationFailure(authentication_error) => {
                authentication_error.into_response()
            }
            Error::NotFound(not_found_error) => not_found_error.into_response(),
        }
    }
}

impl From<ValidationErrors> for Error {
    fn from(err: ValidationErrors) -> Self {
        Error::FieldValidationError(FieldValidationErrors { fields: err.into() })
    }
}

impl From<askama::Error> for Error {
    fn from(value: askama::Error) -> Self {
        Error::new_something_went_wrong(format!("Error while rendering template: {value:?}"))
    }
}

#[cfg(feature = "diesel")]
impl From<diesel::r2d2::PoolError> for Error {
    fn from(value: diesel::r2d2::PoolError) -> Self {
        Error::new_something_went_wrong(format!("{} {:?}", value, value))
    }
}

#[cfg(feature = "diesel")]
impl From<diesel::result::Error> for Error {
    fn from(value: diesel::result::Error) -> Self {
        Error::new_something_went_wrong(format!("{} {:?}", value, value))
    }
}
