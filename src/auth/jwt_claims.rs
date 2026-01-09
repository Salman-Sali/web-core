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

#[derive(Debug, serde::Deserialize)]
pub struct JwtClaims<T> {
    pub id: String,
    pub exp: usize, // Required (validate_exp defaults to true in validation). Expiration time (as UTC timestamp)
    pub aud: Option<String>, // Optional. Audience
    pub iat: usize, // Optional. Issued at (as UTC timestamp)
    pub iss: Option<String>, // Optional. Issuer
    pub nbf: usize, // Optional. Not Before (as UTC timestamp)
    pub sub: String,
    pub purpose: String,
    #[serde(flatten)]
    pub additional_claims: Option<T>,
}

#[doc(hidden)]
#[allow(
    non_upper_case_globals,
    unused_attributes,
    unused_qualifications,
    clippy::absolute_paths
)]
const _: () = {
    #[allow(unused_extern_crates, clippy::useless_attribute)]
    extern crate serde as _serde;
    _serde::__require_serde_not_serde_core!();
    #[automatically_derived]
    impl<T> _serde::Serialize for JwtClaims<T>
    where
        T: _serde::Serialize,
    {
        fn serialize<__S>(
            &self,
            __serializer: __S,
        ) -> _serde::__private228::Result<__S::Ok, __S::Error>
        where
            __S: _serde::Serializer,
        {
            let mut __serde_state =
                _serde::Serializer::serialize_map(__serializer, _serde::__private228::None)?;
            _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "id", &self.id)?;
            _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "exp", &self.exp)?;
            if let Some(aud) = &self.aud {
                _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "aud", aud)?;
            }
            _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "iat", &self.iat)?;
            if let Some(iss) = &self.iss {
                _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "iss", iss)?;
            }
            _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "nbf", &self.nbf)?;
            _serde::ser::SerializeMap::serialize_entry(&mut __serde_state, "sub", &self.sub)?;
            _serde::ser::SerializeMap::serialize_entry(
                &mut __serde_state,
                "purpose",
                &self.purpose,
            )?;
            if let Some(additional_claims) = &self.additional_claims {
                _serde::Serialize::serialize(
                    additional_claims,
                    _serde::__private228::ser::FlatMapSerializer(&mut __serde_state),
                )?;
            }
            _serde::ser::SerializeMap::end(__serde_state)
        }
    }
};

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
