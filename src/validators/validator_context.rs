use std::{any::Any, collections::HashMap};

use axum::{body::Bytes, extract::FromRequestParts};
use http::request::Parts;
use serde::de::DeserializeOwned;

use crate::{axum::PathValueExtractor, error::Error, something_went_wrong};

pub struct ValidatorContext<S: Send + Sync + Clone> {
    parts: Parts,
    state: S,
    bytes: Bytes,
    memo: HashMap<String, Box<dyn Any + Send + Sync>>,
}

impl<S: Send + Sync + Clone> ValidatorContext<S> {
    pub async fn get_path_value<T: DeserializeOwned + Send>(&self) -> Result<T, Error> {
        self.parts.get_path_value(&self.state).await
    }

    pub fn parts(&self) -> &Parts {
        &self.parts
    }

    pub async fn from_request_parts<T>(&mut self) -> Result<T, T::Rejection>
    where
        T: FromRequestParts<S>,
    {
        T::from_request_parts(&mut self.parts, &self.state).await
    }

    pub fn from_body<T: DeserializeOwned>(&self) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.bytes).map_err(|e| something_went_wrong!("{:?}", e))?)
    }

    pub fn state(&self) -> &S {
        &self.state
    }

    pub fn insert<T: Send + Sync + 'static>(&mut self, key: &str, item: T) {
        self.memo.insert(key.to_string(), Box::new(item));
    }

    pub fn get<T: Send + Sync + 'static>(&self, key: &str) -> Option<&T> {
        self.memo.get(key).and_then(|v| v.downcast_ref::<T>())
    }

    pub fn pop<T: Send + Sync + 'static>(&mut self, key: &str) -> Option<T> {
        self.memo
            .remove(key)
            .and_then(|v| v.downcast::<T>().ok().map(|x| *x))
    }
}

impl<S: Send + Sync + Clone> From<(Parts, Bytes, S)> for ValidatorContext<S> {
    fn from(value: (Parts, Bytes, S)) -> Self {
        Self {
            parts: value.0,
            bytes: value.1,
            state: value.2,
            memo: HashMap::new(),
        }
    }
}
