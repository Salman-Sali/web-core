use std::time::Instant;

use axum::{body::Body, http::Request, middleware::{self, Next}, response::Response, Router};

use crate::web_core::WebCoreState;

pub async fn logging_middleware(req: Request<Body>, next: Next) -> Response {
    let start = Instant::now();

    let method = req.method().clone();
    let uri = req.uri().clone();

    let headers = req.headers().clone();

    let client_ip = headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown");

    let response = next.run(req).await;

    let status = response.status();
    let elapsed = start.elapsed();

    println!(
        "[{}] {} {} from {} in {:?}",
        status, method, uri, client_ip, elapsed
    );

    response
}

pub trait LoggingMiddlewareLayer {
    fn with_logging_layer(self)  -> Self;
}

impl<T: Clone + Send + Sync + 'static> LoggingMiddlewareLayer for Router<WebCoreState<T>> {
    fn with_logging_layer(self)  -> Self {
        self.layer(middleware::from_fn(logging_middleware))
    }
}