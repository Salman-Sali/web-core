use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::Request,
    middleware::{self, Next},
    response::Response,
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use reqwest::StatusCode;

use crate::web_core::WebCoreState;

use super::{
    auth_service::{AuthService, TokenPurpose},
    authenticated_user::AuthenticatedUser,
    jwt_claims::JwtClaims,
};

pub async fn authentication_middleware(
    TypedHeader(Authorization(bearer)): TypedHeader<Authorization<Bearer>>,
    mut req: Request<Body>,
    next: Next,
    auth_service: Arc<AuthService>,
) -> Result<Response, StatusCode> {
    let auth_service = auth_service.clone();
    let claims: JwtClaims<()> = auth_service
        .decode_token::<_>(&bearer.token().to_string(), TokenPurpose::Access)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    req.extensions_mut()
        .insert(claims.additional_claims.clone());
    let authenticated_user: AuthenticatedUser = claims.into();
    req.extensions_mut().insert(authenticated_user);

    Ok(next.run(req).await)
}

pub trait AuthMiddlewareLayer {
    fn with_auth_layer(self, auth_service: Arc<AuthService>) -> Self;
}

impl<T> AuthMiddlewareLayer for Router<WebCoreState<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn with_auth_layer(self, auth_service: Arc<AuthService>) -> Self {
        self.layer(middleware::from_fn(move |typed_header: TypedHeader<Authorization<Bearer>>, req: Request<Body>, next: Next| {
            let auth_service = auth_service.clone();
            async move { authentication_middleware(typed_header, req, next, auth_service).await }
        }))
    }
}
