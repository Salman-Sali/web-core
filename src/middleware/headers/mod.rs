use crate::web_core::WebCoreState;
use axum::Router;

pub trait EnsureHeaderValueExists<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Api will return 403 if header and value does not exists in request.
    /// This will not return 403 on features 'local' or 'test_mode'
    fn ensure_header_value_exists(self, key: &str, value: &str) -> Router<WebCoreState<T>>;
}

impl<T> EnsureHeaderValueExists<T> for Router<WebCoreState<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn ensure_header_value_exists(self, key: &str, value: &str) -> Router<WebCoreState<T>> {
        #[cfg(not(any(feature = "local", feature = "test_mode")))]
        {
            use crate::web_core::WebCore;
            use axum::{http::StatusCode, response::IntoResponse};

            let key: String = key.into();
            let value: String = value.into();

            self.with_middleware(move |request, next| {
                let key = key.clone();
                let value = value.clone();
                async move {
                    if let Some(header_value) = request.headers().get(&key) {
                        if let Ok(header_value) = header_value.to_str() {
                            if header_value == value {
                                return next.run(request).await;
                            }
                        }
                    }
                    return StatusCode::FORBIDDEN.into_response();
                }
            })
        }

        #[cfg(any(feature = "local", feature = "test_mode"))]
        self
    }
}
