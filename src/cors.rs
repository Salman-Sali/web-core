use axum::Router;
use axum::http::{
    HeaderValue, Method,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use tower_http::cors::CorsLayer;

use crate::web_core::WebCoreState;

pub trait WithCorsLayer {
    fn with_cors_layer(self, frontend_url: Option<String>) -> Self;
}

impl<T> WithCorsLayer for Router<WebCoreState<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn with_cors_layer(self, frontend_url: Option<String>) -> Self {
        self.layer(generate_cors(frontend_url))
    }
}

pub fn generate_cors(frontend_url: Option<String>) -> CorsLayer {
    #[allow(unused_mut)]
    let mut cors = CorsLayer::new()
        .allow_credentials(false)
        .allow_headers([ACCEPT, AUTHORIZATION, CONTENT_TYPE])
        .allow_methods([
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::GET,
            Method::OPTIONS,
        ]);

    if let Some(frontend_url) = &frontend_url {
        let frontend_url = frontend_url.trim_end_matches("/");
        let frontend_url_2 = if frontend_url.contains("https://www.") {
            frontend_url.replace("https://www.", "https://")
        } else {
            frontend_url.replace("https://", "https://www.")
        };

        cors = cors.allow_origin([
            frontend_url_2.parse::<HeaderValue>().unwrap(),
            frontend_url.parse::<HeaderValue>().unwrap(),
        ]);
    }

    #[cfg(feature = "local")]
    {
        use tower_http::cors::Any;
        cors = cors.allow_origin(Any).allow_credentials(false);
    }
    return cors;
}
