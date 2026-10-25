use std::borrow::Cow;

use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
use diesel::result::{DatabaseErrorInformation, DatabaseErrorKind, Error as DieselError};
use web_core::{conflict, error::Error};
use web_core_validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

struct Info {
    constraint: Option<&'static str>,
}

impl DatabaseErrorInformation for Info {
    fn message(&self) -> &str {
        "duplicate key value violates unique constraint"
    }
    fn details(&self) -> Option<&str> {
        None
    }
    fn hint(&self) -> Option<&str> {
        None
    }
    fn table_name(&self) -> Option<&str> {
        None
    }
    fn column_name(&self) -> Option<&str> {
        None
    }
    fn constraint_name(&self) -> Option<&str> {
        self.constraint
    }
    fn statement_position(&self) -> Option<i32> {
        None
    }
}

async fn response_parts(error: Error) -> (StatusCode, serde_json::Value) {
    let response = error.into_response();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn unique_violation_maps_to_conflict_with_constraint() {
    let error: Error = DieselError::DatabaseError(
        DatabaseErrorKind::UniqueViolation,
        Box::new(Info {
            constraint: Some("users_email_key"),
        }),
    )
    .into();
    assert!(matches!(error, Error::Conflict(_)));
    let (status, body) = response_parts(error).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        serde_json::json!({"error": "Conflict on users_email_key"})
    );
}

#[tokio::test]
async fn unique_violation_without_constraint_maps_to_plain_conflict() {
    let error: Error = DieselError::DatabaseError(
        DatabaseErrorKind::UniqueViolation,
        Box::new(Info { constraint: None }),
    )
    .into();
    let (status, body) = response_parts(error).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, serde_json::json!({"error": "Conflict"}));
}

#[tokio::test]
async fn other_diesel_errors_stay_500() {
    let error: Error = DieselError::DatabaseError(
        DatabaseErrorKind::ForeignKeyViolation,
        Box::new(Info {
            constraint: Some("fk"),
        }),
    )
    .into();
    assert!(matches!(error, Error::SomethingWentWrong(_)));
    let error: Error = DieselError::NotFound.into();
    let (status, _) = response_parts(error).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn conflict_macro_builds_409() {
    let (status, body) = response_parts(conflict!("Order {} already paid", 42)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, serde_json::json!({"error": "Order 42 already paid"}));
    let (status, body) = response_parts(conflict!()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, serde_json::json!({"error": "Conflict"}));
}

#[test]
fn validation_error_internal_constructor() {
    let error = ValidationError::internal(std::io::Error::other("db down"));
    assert!(error.is_internal());
    assert_eq!(error.code, "__internal__");
    assert_eq!(error.message.as_deref(), Some("db down"));
    assert!(!ValidationError::new("length").is_internal());
}

#[tokio::test]
async fn internal_validation_error_maps_to_500() {
    let mut errors = ValidationErrors::new();
    errors.add("email", ValidationError::new("email"));
    errors.add("username", ValidationError::internal("connection refused"));
    let error: Error = errors.into();
    match &error {
        Error::SomethingWentWrong(e) => assert!(e.error_details.contains("connection refused")),
        other => panic!("expected SomethingWentWrong, got {other:?}"),
    }
    let (status, _) = response_parts(error).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn nested_internal_validation_error_maps_to_500() {
    let mut inner = ValidationErrors::new();
    inner.add("sku", ValidationError::internal("timeout"));
    let mut errors = ValidationErrors::new();
    errors.0.insert(
        Cow::Borrowed("items"),
        ValidationErrorsKind::List([(0usize, Box::new(inner))].into_iter().collect()),
    );
    let error: Error = errors.into();
    assert!(matches!(error, Error::SomethingWentWrong(_)));
}

#[tokio::test]
async fn regular_validation_errors_stay_400() {
    let mut errors = ValidationErrors::new();
    errors.add(
        "email",
        ValidationError::new("email").with_message("Invalid email".into()),
    );
    let error: Error = errors.into();
    let (status, body) = response_parts(error).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["fields"]["email"], "Invalid email");
}
