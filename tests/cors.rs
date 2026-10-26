use std::time::Duration;

use axum::{Router, body::Body, http::Request, routing::get};
use http::{HeaderValue, header::ACCESS_CONTROL_ALLOW_ORIGIN, header::ORIGIN, request::Parts};
use lambda_http::tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use web_core::{
    auth::{auth_options::AuthOptions, auth_service::AuthService},
    web_core::{WebCore, WebCoreOptions, WebCoreState},
};

fn options() -> WebCoreOptions<()> {
    let auth = AuthService::new(AuthOptions::new(
        "secret".to_string(),
        Duration::from_secs(60),
        Duration::from_secs(60),
    ));
    WebCoreOptions::new(WebCoreState::new(auth, ()))
}

fn app(options: WebCoreOptions<()>) -> Router {
    Router::<WebCoreState<()>>::new()
        .route("/ping", get(|| async { "pong" }))
        .with_web_core(options)
}

/// `Access-Control-Allow-Origin` of a GET with `Origin: origin`.
async fn allowed_origin(app: Router, origin: &str) -> Option<String> {
    let request = Request::builder()
        .uri("/ping")
        .header(ORIGIN, origin)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    response
        .headers()
        .get(ACCESS_CONTROL_ALLOW_ORIGIN)
        .map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test]
async fn frontend_url_default_is_kept() {
    let app = app(options().with_frontend_url("https://shop.example.com/".to_string()));
    let allowed = allowed_origin(app.clone(), "https://www.shop.example.com").await;
    if cfg!(debug_assertions) {
        // Debug builds allow every origin.
        assert_eq!(allowed.as_deref(), Some("*"));
    } else {
        assert_eq!(allowed.as_deref(), Some("https://www.shop.example.com"));
        assert_eq!(allowed_origin(app, "https://evil.example").await, None);
    }
}

#[tokio::test]
async fn async_predicate_decides_allowed_origins() {
    let known = ["https://mycar.com", "https://shop.storelo.in"];
    let allow = AllowOrigin::async_predicate(move |origin: HeaderValue, _: &Parts| async move {
        // Stand-in for a tenant host cache lookup.
        tokio::task::yield_now().await;
        known.iter().any(|k| origin.as_bytes() == k.as_bytes())
    });
    let app = app(options()
        .with_frontend_url("https://ignored.example".to_string())
        .with_allowed_origin(allow));

    assert_eq!(
        allowed_origin(app.clone(), "https://mycar.com")
            .await
            .as_deref(),
        Some("https://mycar.com")
    );
    assert_eq!(
        allowed_origin(app.clone(), "https://shop.storelo.in")
            .await
            .as_deref(),
        Some("https://shop.storelo.in")
    );
    // Not allowed, even in debug builds and even if it was a frontend URL.
    assert_eq!(
        allowed_origin(app.clone(), "https://evil.example").await,
        None
    );
    assert_eq!(allowed_origin(app, "https://ignored.example").await, None);
}

#[tokio::test]
async fn custom_cors_layer_replaces_default() {
    let layer = CorsLayer::new().allow_origin(HeaderValue::from_static("https://a.example"));
    let app = app(options().with_cors_layer(layer));
    assert_eq!(
        allowed_origin(app.clone(), "https://a.example")
            .await
            .as_deref(),
        Some("https://a.example")
    );
    // An exact origin is always sent as is; the browser rejects the mismatch.
    assert_eq!(
        allowed_origin(app, "https://b.example").await.as_deref(),
        Some("https://a.example")
    );
}
