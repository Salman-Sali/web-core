use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;
use web_core_validator::{HasContext, Validate, ValidateArgs};

use crate::error::Error;

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
        + for<'v_a> ValidateArgs<'v_a, Args = &'v_a S>
        + Send
        + HasContext<S>
        + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e: JsonRejection| (StatusCode::BAD_REQUEST, e.to_string()).into_response())?;

        value
            .validate_with_args(state)
            .await
            .map_err(|e| Error::from(e).into_response())?;

        Ok(ValidatedJsonWithCtx(value))
    }
}
