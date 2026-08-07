use axum::{
    extract::{FromRef, FromRequestParts},
    http::request::Parts,
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use chrono::Utc;
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;
use uuid::Uuid;

use crate::{error::Error, something_went_wrong, web_core::WebCoreState};

use super::{auth_service::TokenPurpose, authenticated_user::AuthenticatedUser};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct JwtClaims<T> {
    pub id: String,
    pub exp: usize, // Required (validate_exp defaults to true in validation). Expiration time (as UTC timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>, // Optional. Audience
    pub iat: usize, // Optional. Issued at (as UTC timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>, // Optional. Issuer
    pub nbf: usize, // Optional. Not Before (as UTC timestamp)
    pub sub: String,
    pub purpose: String,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub additional_claims: Option<T>,
}

impl<T> JwtClaims<T> {
    pub fn new(
        aud: Option<String>,
        iss: Option<String>,
        sub: String,
        purpose: TokenPurpose,
        additional_claims: Option<T>,
        exp_duration: Duration,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            exp: (now + exp_duration).timestamp() as usize,
            aud,
            iat: now.timestamp() as usize,
            iss,
            nbf: now.timestamp() as usize,
            sub,
            purpose: purpose.to_string(),
            additional_claims,
        }
    }
}

impl<T, S> FromRequestParts<S> for JwtClaims<T>
where
    S: Send + Sync,
    T: Serialize + DeserializeOwned,
    WebCoreState<()>: FromRef<S>,
{
    type Rejection = Error;
    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        async move {
            let TypedHeader(Authorization(bearer)) =
                TypedHeader::<Authorization<Bearer>>::from_request_parts(parts, state)
                    .await
                    .map_err(|e| {
                        something_went_wrong!("Error while getting bearer token : {:?}", e)
                    })?;

            let state = WebCoreState::<()>::from_ref(state);

            state
                .auth_service
                .decode_token(bearer.token(), TokenPurpose::Access)
        }
    }
}

impl<T> Into<AuthenticatedUser> for JwtClaims<T> {
    fn into(self) -> AuthenticatedUser {
        AuthenticatedUser::new(self.sub.clone())
    }
}
