use http::request::Parts;
use serde::de::DeserializeOwned;

use crate::{axum::PathValueExtractor, error::Error};

pub struct ValidatorContext<S: Send + Sync + Clone> {
    pub parts: Parts,
    pub state: S,
}

impl<S: Send + Sync + Clone> ValidatorContext<S> {
    pub async fn get_path_value<T: DeserializeOwned + Send>(&self) -> Result<T, Error> {
        self.parts.get_path_value(&self.state).await
    }
}

impl<S: Send + Sync + Clone> From<(Parts, S)> for ValidatorContext<S> {
    fn from(value: (Parts, S)) -> Self {
        Self {
            parts: value.0,
            state: value.1,
        }
    }
}
