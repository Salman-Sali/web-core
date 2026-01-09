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
use validator_async::{ValidationError, ValidationErrors};

#[derive(Debug)]
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
        let mut hash_map = HashMap::new();
        hash_map.insert(field.into(), error.into());
        let field_validation_errors = FieldValidationErrors { fields: hash_map };
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
            Error::FieldValidationError(field_validation_errors) => (StatusCode::BAD_REQUEST, Json(field_validation_errors)).into_response(),
            Error::BadRequestError(bad_request_error) => (StatusCode::BAD_REQUEST, Json(serde_json::json!(bad_request_error))).into_response(),
            Error::SomethingWentWrong(something_went_wrong) => {
                something_went_wrong.print_error();
                something_went_wrong.into_response()
            }
            Error::AuthenticationFailure(authentication_error) => authentication_error.into_response(),
            Error::NotFound(not_found_error) => not_found_error.into_response(),
        }
    }
}

impl From<ValidationErrors> for Error {
    fn from(err: ValidationErrors) -> Self {
        let mut fields: HashMap<String, String> = HashMap::new();
        for error in err.field_errors() {
            let field = error.0.to_string();
            let errors: Vec<String> = error.1.iter().map(|x| x.to_string()).collect();
            if field == "__all__" {
                return Error::BadRequestError(BadRequestError::new(errors.join(" ")));
            }
            fields.insert(field, errors.join(" "));
        }
        Error::FieldValidationError(FieldValidationErrors { fields })
    }
}

impl From<ValidationError> for Error {
    fn from(err: ValidationError) -> Self {
        let mut fields: HashMap<String, String> = HashMap::new();
        if err.code == "__all__" {
            return Error::BadRequestError(BadRequestError::new(err.message.unwrap().to_string()));
        }
        fields.insert(err.code.to_string(), err.message.unwrap().to_string());
        Error::FieldValidationError(FieldValidationErrors { fields })
    }
}
