#![cfg(feature = "diesel")]

use std::fmt::Debug;

#[web_core_derive::diesel_jsonb]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JsonbData<T: Debug + Clone>(pub T);
