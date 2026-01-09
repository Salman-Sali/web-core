use std::{
    fmt::{Debug, Display},
    usize,
};

use async_trait::async_trait;
use axum::{Router, body::to_bytes, http::Request};
use http::StatusCode;
use lambda_http::{Body, tower::ServiceExt};

pub trait JsonType:
    serde::Serialize + serde::de::DeserializeOwned + Debug + Send + Sync + 'static
{
}
impl<T> JsonType for T where
    T: serde::Serialize + serde::de::DeserializeOwned + Debug + Send + Sync + 'static
{
}

#[derive(Debug, Clone)]
pub struct ErrorResponse<E: JsonType> {
    pub status: StatusCode,
    pub deserialised_error: E,
}

impl<E: JsonType> Display for ErrorResponse<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "Status Code : {}\nError: {:?}",
            self.status, self.deserialised_error
        ))
    }
}
impl<E: JsonType> ErrorResponse<E> {
    pub fn new(status: StatusCode, error: E) -> Self {
        Self {
            status,
            deserialised_error: error,
        }
    }
}

#[async_trait]
pub trait RouterExtensions {
    async fn post_api<T, R, E>(self, path: &str, request: Option<T>) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType;

    async fn post_api_with_access_token<T, R, E>(
        self,
        path: &str,
        access_token: String,
        request: Option<T>,
    ) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType;

    async fn put_api<T, R, E>(
        self,
        path: &str,
        access_token: String,
        request: T,
    ) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType;

    async fn get_api<R, E>(self, path: &str) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType;

    async fn get_api_with_access_token<R, E>(
        self,
        path: &str,
        access_token: String,
    ) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType;

    async fn delete_api<E>(self, path: &str, access_token: String) -> Result<(), ErrorResponse<E>>
    where
        E: JsonType;

    async fn one_shot<R, E>(self, request: Request<Body>) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType;

    async fn one_shot_without_result<E>(
        self,
        request: Request<Body>,
    ) -> Result<(), ErrorResponse<E>>
    where
        E: JsonType;
}

#[async_trait]
impl RouterExtensions for Router {
    async fn post_api_with_access_token<T, R, E>(
        self,
        path: &str,
        access_token: String,
        request: Option<T>,
    ) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType,
    {
        let request_builder = Request::builder()
            .method("POST")
            .uri(path)
            .header("Authorization", format!("Bearer {access_token}"));

        let request = if let Some(r) = request {
            request_builder
                .header("content-type", "application/json")
                .body(Body::Text(
                    serde_json::to_string(&r).expect("Error while converting request into json."),
                ))
        } else {
            request_builder.body(Body::Empty)
        }
        .expect("Error while creating request.");
        self.one_shot::<R, E>(request).await
    }

    async fn post_api<T, R, E>(self, path: &str, request: Option<T>) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType,
    {
        let request_builder = Request::builder().method("POST").uri(path);

        let request = if let Some(r) = request {
            request_builder
                .header("content-type", "application/json")
                .body(Body::Text(
                    serde_json::to_string(&r).expect("Error while converting request into json."),
                ))
        } else {
            request_builder.body(Body::Empty)
        }
        .expect("Error while creating request.");
        self.one_shot::<R, E>(request).await
    }
    async fn put_api<T, R, E>(
        self,
        path: &str,
        access_token: String,
        request: T,
    ) -> Result<R, ErrorResponse<E>>
    where
        T: JsonType,
        R: JsonType,
        E: JsonType,
    {
        let request = Request::builder()
            .method("PUT")
            .uri(path)
            .header("content-type", "application/json")
            .header("Authorization", format!("Bearer {access_token}"))
            .body(Body::Text(
                serde_json::to_string(&request).expect("Error while converting request into json."),
            ))
            .expect("Error while creating request.");
        self.one_shot::<R, E>(request).await
    }

    async fn get_api<R, E>(self, path: &str) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType,
    {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::Empty)
            .expect("Error while creating request.");
        self.one_shot(request).await
    }

    async fn get_api_with_access_token<R, E>(
        self,
        path: &str,
        access_token: String,
    ) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType,
    {
        let request = Request::builder()
            .method("GET")
            .uri(path)
            .header("Authorization", format!("Bearer {access_token}"))
            .body(Body::Empty)
            .expect("Error while creating request.");
        self.one_shot(request).await
    }

    async fn delete_api<E>(self, path: &str, access_token: String) -> Result<(), ErrorResponse<E>>
    where
        E: JsonType,
    {
        let request = Request::builder()
            .method("DELETE")
            .uri(path)
            .header("Authorization", format!("Bearer {access_token}"))
            .body(Body::Empty)
            .expect("Error while creating request.");
        self.one_shot_without_result::<E>(request).await
    }

    async fn one_shot<R, E>(self, request: Request<Body>) -> Result<R, ErrorResponse<E>>
    where
        R: JsonType,
        E: JsonType,
    {
        let response = self
            .oneshot(request)
            .await
            .expect("Error while sending post request.");

        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("Error while converting response body into bytes.");

        return if status.is_success() {
            Ok(serde_json::from_slice(&bytes)
                .expect("Error while converting json bytes into struct object."))
        } else {
            println!("{:?}", bytes);
            let body = serde_json::from_slice(&bytes)
                .expect("Error while converting json bytes into struct object.");
            Err(ErrorResponse::new(status, body))
        };
    }

    async fn one_shot_without_result<E>(
        self,
        request: Request<Body>,
    ) -> Result<(), ErrorResponse<E>>
    where
        E: JsonType,
    {
        let response = self
            .oneshot(request)
            .await
            .expect("Error while sending post request.");

        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("Error while converting response body into bytes.");

        return if status.is_success() {
            Ok(())
        } else {
            println!("{:?}", bytes);
            let body = serde_json::from_slice(&bytes)
                .expect("Error while converting json bytes into struct object.");
            Err(ErrorResponse::new(status, body))
        };
    }
}
