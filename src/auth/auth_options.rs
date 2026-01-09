use jsonwebtoken::{DecodingKey, EncodingKey, Header};
use std::time::Duration;

use super::{auth_service::TokenPurpose, jwt_claims::JwtClaims};

#[derive(Clone)]
pub struct AuthOptions {
    pub encoding_key: EncodingKey,
    pub decoding_key: DecodingKey,
    pub header: Header,
    pub audience: Option<String>,
    pub access_token_lifetime: Duration,
    pub refresh_token_lifetime: Duration,
}

impl AuthOptions {
    pub fn new(
        secret: String,
        access_token_lifetime: Duration,
        refresh_token_lifetime: Duration,
    ) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            header: Header::new(jsonwebtoken::Algorithm::HS512),
            audience: None,
            access_token_lifetime,
            refresh_token_lifetime,
        }
    }

    pub fn with_audience(mut self, audience: String) -> Self {
        self.audience = Some(audience);
        self
    }

    pub(crate) fn generate_claim(&self, subject: String, purpose: TokenPurpose) -> JwtClaims<()> {
        let lifetime = match &purpose {
            TokenPurpose::Access => self.access_token_lifetime,
            TokenPurpose::Refresh => self.refresh_token_lifetime,
            TokenPurpose::Other {
                purpose: _,
                lifetime,
            } => *lifetime,
        }
        .clone();
        JwtClaims::new(
            self.audience.clone(),
            self.audience.clone(),
            subject,
            purpose,
            None,
            lifetime,
        )
    }
}
