use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;
use web_core_validator::{HasContext, Validate, ValidateArgs};

use crate::{ValidatorContext, error::Error};

pub struct ValidatedJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned + Validate + Send + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e: JsonRejection| (StatusCode::BAD_REQUEST, e.to_string()).into_response())?;

        value
            .validate()
            .await
            .map_err(|e| Error::from(e).into_response())?;

        Ok(ValidatedJson(value))
    }
}

pub struct ValidatedJsonWithCtx<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJsonWithCtx<T>
where
    T: DeserializeOwned
        + for<'v_a> ValidateArgs<'v_a, Args = ValidatorContext<S>>
        + Send
        + HasContext<ValidatorContext<S>>
        + 'static,
    S: Send + Sync + Clone,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let bytes = axum::body::to_bytes(body, 1024 * 1024)
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "Failed to read body").into_response())?;

        let value: T = serde_json::from_slice(&bytes)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()).into_response())?;

        let mut context = ValidatorContext::from((parts, state.clone()));
        value
            .validate_with_args(&mut context)
            .await
            .map_err(|e| Error::from(e).into_response())?;

        Ok(ValidatedJsonWithCtx(value))
    }
}
