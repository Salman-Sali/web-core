use async_trait::async_trait;
use axum::extract::{FromRequestParts, Path};
use http::request::Parts;
use serde::de::DeserializeOwned;

use crate::{bad_request, error::Error};

#[async_trait]
pub trait PathValueExtractor {
    async fn get_path_value<T, S>(&self, state: &S) -> Result<T, Error>
    where
        T: DeserializeOwned + Send,
        S: Send + Sync + Clone;
}

#[async_trait]
impl PathValueExtractor for Parts {
    async fn get_path_value<T, S>(&self, state: &S) -> Result<T, Error>
    where
        T: DeserializeOwned + Send,
        S: Send + Sync + Clone,
    {
        let mut parts = self.clone();
        let Path(x) = Path::<T>::from_request_parts(&mut parts, &state)
            .await
            .map_err(|_| bad_request!("Invalid path id."))?;
        return Ok(x);
    }
}
