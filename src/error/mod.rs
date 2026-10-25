pub mod authentication;
pub mod bad_request;
pub mod conflict;
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
use conflict::ConflictError;
use field_validation::FieldValidationErrors;
use not_found::NotFoundError;
use something_went_wrong::SomethingWentWrong;
use web_core_validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

use crate::error::field_validation::{FieldErrorKind, FieldErrors};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Error {
    FieldValidationError(FieldValidationErrors),
    BadRequestError(BadRequestError),
    SomethingWentWrong(SomethingWentWrong),
    AuthenticationFailure(AuthenticationError),
    NotFound(NotFoundError),
    Conflict(ConflictError),
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

    pub fn new_conflict(message: &str) -> Error {
        Error::Conflict(ConflictError::new(message.to_string()))
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
            Error::Conflict(conflict_error) => conflict_error.into_response(),
        }
    }
}

/// Converts validation failures into an API error (used by `ValidatedJson` / `ValidatedJsonWithCtx`).
///
/// Normally this is a 400 [`Error::FieldValidationError`]. If any field error anywhere in the
/// tree (including nested structs and lists) was created with [`ValidationError::internal`]
/// (`is_internal()`), the result is instead [`Error::SomethingWentWrong`] (500) carrying that
/// error's message. This lets a validator that talks to a database (or anything else that can
/// fail) report an infrastructure failure as a real 500 instead of unwrapping or returning a
/// misleading 400:
///
/// ```rust,ignore
/// async fn unique_email(email: &str, ctx: &mut ValidatorContext<AppState>) -> Result<(), ValidationError> {
///     let taken = email_exists(ctx.state(), email).await.map_err(ValidationError::internal)?;
///     if taken { return Err(("email_taken", "Email is already registered").into()); }
///     Ok(())
/// }
/// ```
impl From<ValidationErrors> for Error {
    fn from(err: ValidationErrors) -> Self {
        if let Some(internal) = find_internal_error(&err) {
            return crate::something_went_wrong!(
                "Internal error during validation: {}",
                internal.message.as_deref().unwrap_or_default()
            );
        }
        Error::FieldValidationError(FieldValidationErrors { fields: err.into() })
    }
}

fn find_internal_error(errors: &ValidationErrors) -> Option<&ValidationError> {
    errors.0.values().find_map(|kind| match kind {
        ValidationErrorsKind::Field(errors) => errors.iter().find(|e| e.is_internal()),
        ValidationErrorsKind::Struct(nested) => find_internal_error(nested),
        ValidationErrorsKind::List(items) => items.values().find_map(|e| find_internal_error(e)),
    })
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

#[cfg(feature = "diesel-async")]
impl From<diesel_async::pooled_connection::deadpool::PoolError> for Error {
    fn from(value: diesel_async::pooled_connection::deadpool::PoolError) -> Self {
        Error::new_something_went_wrong(format!("{} {:?}", value, value))
    }
}

/// Unique-constraint violations become [`Error::Conflict`] (409) with the message
/// `"Conflict on <constraint>"` (or `"Conflict"` when Postgres reports no constraint name).
/// Every other Diesel error becomes [`Error::SomethingWentWrong`] (500).
#[cfg(feature = "diesel")]
impl From<diesel::result::Error> for Error {
    fn from(value: diesel::result::Error) -> Self {
        use diesel::result::{DatabaseErrorKind, Error as DieselError};
        match value {
            DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, info) => {
                match info.constraint_name() {
                    Some(constraint) => Error::new_conflict(&format!("Conflict on {constraint}")),
                    None => Error::new_conflict("Conflict"),
                }
            }
            value => Error::new_something_went_wrong(format!("{} {:?}", value, value)),
        }
    }
}
