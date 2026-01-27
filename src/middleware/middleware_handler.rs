use std::pin::Pin;

use axum::{
    body::Body,
    extract::Request,
    middleware::{self, FromFnLayer, Next},
    response::Response,
};

pub fn crate_middleware_handler<F, Fut>(
    header_handler: F,
) -> FromFnLayer<
    impl Fn(Request<Body>, Next) -> Pin<Box<dyn Future<Output = Response> + Send>> + Clone,
    (),
    (Request<Body>,),
>
where
    F: Fn(Request<Body>, Next) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    let middleware = move |req: Request<Body>, next: Next| {
        let handler = header_handler.clone();

        let fut = async move { handler(req, next).await };
        Box::pin(fut) as Pin<Box<dyn Future<Output = Response> + Send>>
    };

    middleware::from_fn(middleware)
}
