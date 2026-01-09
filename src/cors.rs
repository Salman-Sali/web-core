use axum::Router;
use tower_http::cors::CorsLayer;
use axum::http::{
    HeaderValue, Method,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE}};

use crate::web_core::WebCoreState;

pub trait WithCorsLayer {
    fn with_cors_layer(self, frontend_url: Option<String>) -> Self;
}

impl<T> WithCorsLayer for Router<WebCoreState<T>> where T: Clone + Send + Sync + 'static {
    fn with_cors_layer(self, frontend_url: Option<String>) -> Self {
        self
            .layer(generate_cors(frontend_url))
    }
}



pub fn generate_cors(frontend_url: Option<String>) -> CorsLayer {
    #[allow(unused_mut)]
    let mut cors = CorsLayer::new()
    .allow_credentials(true)
    .allow_headers([ACCEPT, AUTHORIZATION, CONTENT_TYPE])
    .allow_methods([Method::POST, Method::PUT, Method::DELETE, Method::GET]);

    if let Some(frontend_url) = &frontend_url {
        cors = cors
            .allow_origin(frontend_url.parse::<HeaderValue>().unwrap());        
    }

    #[cfg(feature = "local")]
    {
        use tower_http::cors::Any;
        cors = cors
            .allow_origin(Any)
            .allow_credentials(false);
    }
    return cors;
}