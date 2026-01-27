use std::sync::Arc;

use crate::{
    auth::auth_service::AuthService,
    cors::WithCorsLayer,
    middleware::{
        logging_middleware::LoggingMiddlewareLayer, middleware_handler::crate_middleware_handler,
    },
};
use axum::{Router, body::Body, extract::Request, middleware::Next, response::Response};

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
            frontend_url,
        } = options;
        self.with_logging_layer()
            .with_cors_layer(frontend_url)
            .with_state(web_core_state)
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
    frontend_url: Option<String>,
}

impl<T> WebCoreOptions<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub fn new(web_core_state: WebCoreState<T>) -> Self {
        Self {
            web_core_state,
            frontend_url: None,
        }
    }

    pub fn with_frontend_url(mut self, frontend_url: String) -> Self {
        self.frontend_url = Some(frontend_url);
        self
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
            additional_state: additional_state,
        }
    }
}
