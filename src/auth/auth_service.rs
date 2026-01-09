use std::{sync::Arc, time::Duration};

use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use jsonwebtoken::{Validation, crypto, decode};
use serde::Serialize;

use crate::{error::Error, something_went_wrong, unauthorized};

use super::{auth_options::AuthOptions, jwt_claims::JwtClaims};

pub struct AuthService {
    auth_options: AuthOptions,
}

impl AuthService {
    pub fn new(options: AuthOptions) -> Self {
        Self {
            auth_options: options,
        }
    }

    pub fn generate_token(&self, token_options: TokenOptions) -> Result<Token, Error> {
        let TokenOptions {
            subject,
            additional_claims,
            purpose,
        } = token_options;
        let claims = self.auth_options.generate_claim(subject, purpose);
        let token_id = claims.id.clone();
        let claims = generate_encoded_claims(claims, additional_claims)?;
        Ok(Token::new(token_id, self.encode(claims)?))
    }

    pub fn decode_token<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        token: &str,
        purpose: TokenPurpose,
    ) -> Result<JwtClaims<T>, Error> {
        let mut validation = Validation::new(jsonwebtoken::Algorithm::HS512);

        #[cfg(feature = "test_mode")]
        {
            validation.leeway = 0;
        }

        if let Some(audience) = &self.auth_options.audience {
            validation.set_audience(&[audience.to_string()]);
        }
        match decode::<JwtClaims<T>>(token, &self.auth_options.decoding_key, &validation) {
            Ok(x) => {
                if x.claims.purpose != purpose.to_string() {
                    return Err(unauthorized!("Token purpose does not match."));
                }
                return Ok(x.claims);
            }
            Err(e) => Err(unauthorized!("Error while decoding token : {e}")),
        }
    }

    fn encode(&self, claims: String) -> Result<String, Error> {
        let encoded_header = b64_encode_part(&self.auth_options.header)?;
        let encoded_claims = b64_encode(claims.as_bytes());
        let message = [encoded_header, encoded_claims].join(".");
        let signature = crypto::sign(
            message.as_bytes(),
            &self.auth_options.encoding_key,
            self.auth_options.header.alg,
        )
        .map_err(|e| something_went_wrong!("Error while encoding token : {e}"))?;

        Ok([message, signature].join("."))
    }
}

fn generate_encoded_claims(
    claims: JwtClaims<()>,
    additional_claims: Option<String>,
) -> Result<String, Error> {
    let mut json = serde_json::to_string(&claims)
        .map_err(|e| something_went_wrong!("Error while converting claims to json : {e}"))?;

    if let Some(custom_claims) = additional_claims {
        json = [
            json.trim_end_matches("}"),
            custom_claims.trim_start_matches("{"),
        ]
        .join(",");
    }
    return Ok(json);
}

fn b64_encode_part<T: Serialize>(input: &T) -> Result<String, Error> {
    let json = serde_json::to_vec(input)
        .map_err(|e| something_went_wrong!("Error while converting token data to json : {e}"))?;
    Ok(b64_encode(json))
}

fn b64_encode<T: AsRef<[u8]>>(input: T) -> String {
    BASE64_URL_SAFE_NO_PAD.encode(input)
}

#[derive(Clone)]
pub struct TokenOptions {
    subject: String,
    additional_claims: Option<String>,
    purpose: TokenPurpose,
}

impl TokenOptions {
    pub fn new(subject: String, purpose: TokenPurpose) -> Self {
        Self {
            subject,
            additional_claims: None,
            purpose,
        }
    }

    pub fn with_additional_claims<T: serde::Serialize + serde::de::DeserializeOwned>(
        mut self,
        additional_claims: T,
    ) -> Result<Self, Error> {
        let value = serde_json::to_string(&additional_claims).map_err(|e| {
            something_went_wrong!("Error while converting custom claims to json : {e}")
        })?;
        self.additional_claims = Some(value);
        Ok(self)
    }
}

#[derive(strum_macros::Display, Clone)]
pub enum TokenPurpose {
    Access,
    Refresh,
    #[strum(to_string = "{purpose}")]
    Other {
        purpose: String,
        lifetime: Duration,
    },
}

impl TokenPurpose {
    pub const fn new(purpose: String, lifetime: Duration) -> TokenPurpose {
        TokenPurpose::Other { purpose, lifetime }
    }
}

pub trait AuthHandler {
    fn generate_access_token_options(&self) -> TokenOptions;
    fn generate_refresh_token_options(&self) -> TokenOptions;
}

pub trait AuthHandlerExtensions {
    fn generate_tokens(&self, auth_service: Arc<AuthService>) -> Result<Tokens, Error>;
}

pub trait AuthHandlerAdmin {
    fn generate_access_token_options(&self) -> TokenOptions;
}

impl<T> AuthHandlerExtensions for T
where
    T: AuthHandler,
{
    fn generate_tokens(&self, auth_service: Arc<AuthService>) -> Result<Tokens, Error> {
        let access_options = self.generate_access_token_options();
        let refresh_options = self.generate_refresh_token_options();

        Ok(Tokens {
            access_token: auth_service.generate_token(access_options)?,
            refresh_token: auth_service.generate_token(refresh_options)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub id: String,
    pub value: String,
}

impl Token {
    pub fn new(id: String, value: String) -> Self {
        Self { id, value }
    }
}

#[derive(Debug, Clone)]
pub struct Tokens {
    pub access_token: Token,
    pub refresh_token: Token,
}
