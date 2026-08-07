pub mod auth;
pub mod cors;
pub mod error;
pub mod macros;
pub mod middleware;
pub mod reqwest;
pub mod test;
pub mod utils;
pub mod validators;
pub mod web_core;

#[cfg(feature = "aws")]
pub mod aws;
pub mod diesel;

pub use serde_json;

#[cfg(feature = "diesel")]
pub use web_core_derive::diesel_jsonb;
