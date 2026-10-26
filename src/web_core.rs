use std::sync::Arc;

use crate::{
    auth::auth_service::AuthService,
    cors::{WithCorsLayer, base_cors_layer},
    middleware::{
        logging_middleware::LoggingMiddlewareLayer, middleware_handler::crate_middleware_handler,
    },
};
use axum::{Router, body::Body, extract::Request, middleware::Next, response::Response};
use tower_http::cors::{AllowOrigin, CorsLayer};

pub trait WebCore<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn with_web_core(self, options: WebCoreOptions<T>) -> Router;
    fn with_middleware<F, Fut>(self, header_handler: F) -> Router<WebCoreState<T>>
    where
        F: Fn(Request<Body>, Next) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Response> + Send + 'static;
}

impl<T> WebCore<T> for Router<WebCoreState<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn with_web_core(self, options: WebCoreOptions<T>) -> Router {
        let WebCoreOptions {
            web_core_state,
            frontend_urls: frontend_url,
            cors_layer,
        } = options;
        let router = self.with_logging_layer();
        let router = match cors_layer {
            Some(cors_layer) => router.layer(cors_layer),
            None => router.with_cors_layer(frontend_url),
        };
        router.with_state(web_core_state)
    }

    fn with_middleware<F, Fut>(self, handler: F) -> Router<WebCoreState<T>>
    where
        F: Fn(Request<Body>, Next) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler = crate_middleware_handler(handler);
        self.layer(handler)
    }
}

pub struct WebCoreOptions<T>
where
    T: Clone + Send + Sync + 'static,
{
    web_core_state: WebCoreState<T>,
    frontend_urls: Vec<String>,
    cors_layer: Option<CorsLayer>,
}

impl<T> WebCoreOptions<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn new(web_core_state: WebCoreState<T>) -> Self {
        Self {
            web_core_state,
            frontend_urls: vec![],
            cors_layer: None,
        }
    }

    pub fn with_frontend_url(mut self, frontend_url: String) -> Self {
        self.frontend_urls.push(frontend_url);
        self
    }

    /// Uses `cors_layer` instead of the one built from the frontend URLs
    /// ([`with_frontend_url`](Self::with_frontend_url) is then ignored for CORS).
    pub fn with_cors_layer(mut self, cors_layer: CorsLayer) -> Self {
        self.cors_layer = Some(cors_layer);
        self
    }

    /// Keeps web-core's CORS settings ([`base_cors_layer`]) but decides the allowed origins with
    /// `allow_origin`, e.g. `AllowOrigin::async_predicate(...)` looking the origin up in a
    /// tenant cache. Unlike the frontend-URL default, debug builds do not allow every origin.
    pub fn with_allowed_origin(self, allow_origin: impl Into<AllowOrigin>) -> Self {
        self.with_cors_layer(base_cors_layer().allow_origin(allow_origin))
    }
}

#[derive(Clone)]
pub struct WebCoreState<T: Clone + Send + Sync + 'static> {
    pub auth_service: Arc<AuthService>,
    pub additional_state: T,
}

impl<T> WebCoreState<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn new(auth_service: AuthService, additional_state: T) -> Self {
        Self {
            auth_service: Arc::new(auth_service),
            additional_state,
        }
    }
}
