use axum::Router;
use axum::http::{
    HeaderValue, Method,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use tower_http::cors::CorsLayer;

use crate::web_core::WebCoreState;

pub trait WithCorsLayer {
    fn with_cors_layer(self, frontend_url: Vec<String>) -> Self;
}

impl<T> WithCorsLayer for Router<WebCoreState<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn with_cors_layer(self, frontend_url: Vec<String>) -> Self {
        self.layer(generate_cors(frontend_url))
    }
}

pub fn generate_cors(frontend_urls: Vec<String>) -> CorsLayer {
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

    let mut header_values: Vec<HeaderValue> = vec![];

    for url in frontend_urls {
        let frontend_url = url.trim_end_matches("/");
        let frontend_url_2 = if frontend_url.contains("https://www.") {
            frontend_url.replace("https://www.", "https://")
        } else {
            frontend_url.replace("https://", "https://www.")
        };

        header_values.push(frontend_url_2.parse::<HeaderValue>().unwrap());
        header_values.push(frontend_url.parse::<HeaderValue>().unwrap());
    }

    cors = cors.allow_origin(header_values);

    #[cfg(feature = "local")]
    {
        use tower_http::cors::Any;
        cors = cors.allow_origin(Any).allow_credentials(false);
    }
    return cors;
}
