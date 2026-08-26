use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;
use web_core_validator::Validate;

use crate::error::Error;

pub struct ValidatedJson<T>(pub T);

pub trait HasContext<T> {}

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned + Validate<S> + Send + HasContext<S> + 'static,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e: JsonRejection| (StatusCode::BAD_REQUEST, e.to_string()).into_response())?;

        value
            .validate(state)
            .await
            .map_err(|e| Error::from(e).into_response())?;

        Ok(ValidatedJson(value))
    }
}
