# web-core

Core utilities for building web applications with Axum. Provides authentication, authorization, CORS, error handling, testing helpers, and more.

## Installation

```toml
[dependencies]
web-core = { git = "https://github.com/Salman-Sali/web-core.git", tag = "0.1.19" }

[features]
# Enables CORS allow-all for local development
local = ["web-core/local"]

[dev-dependencies]
web-core = { git = "https://github.com/Salman-Sali/web-core.git", tag = "0.1.19", features = ["test_mode"] }
```

### Optional Features

| Feature | Description |
|---------|-------------|
| `aws` | AWS SES email sending and CloudFront utilities |
| `diesel` | Diesel ORM integration with JSONB support |
| `derive` | Procedural macros for derive traits |
| `all` | Enables all features |

## Quick Start

```rust
use std::time::Duration;
use axum::{Router, routing::post, Json, extract::{State, Extension}};
use web_core::{
    auth::{AuthOptions, AuthService, AuthenticatedUser},
    error::Error,
    web_core::{WebCore, WebCoreOptions, WebCoreState},
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    // Configure authentication
    let auth_options = AuthOptions::new(
        String::from("your-secret-key"),
        Duration::from_secs(60 * 60),        // 1 hour access token
        Duration::from_secs(30 * 24 * 60 * 60) // 30 days refresh token
    ).with_audience(String::from("https://your-api.com"));

    // Create web core state with your application state
    let app_state = MyAppState::default();
    let web_core_state = WebCoreState::new(
        AuthService::new(auth_options),
        app_state
    );

    // Configure protected routes with auth middleware
    let protected_routes = Router::new()
        .route("/posts", post(create_post))
        .with_auth_layer(web_core_state.auth_service.clone());

    // Configure web core options
    let web_core_options = WebCoreOptions::new(web_core_state)
        .with_frontend_url(String::from("https://your-frontend.com"));

    // Build application
    let app = Router::new()
        .merge(protected_routes)
        .with_web_core(web_core_options);

    // Run server
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// Example protected handler
async fn create_post(
    State(state): State<WebCoreState<MyAppState>>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(payload): Json<CreatePostRequest>,
) -> Result<Json<CreatePostResponse>, Error> {
    // user.subject contains the authenticated user's ID
    // Access app state via state.additional_state
    todo!()
}
```

## Features

### Authentication & Authorization

The crate provides JWT-based authentication with HS512 signing:

```rust
use web_core::auth::{
    AuthOptions, AuthService, TokenOptions, TokenPurpose,
    AuthHandler, AuthHandlerExtensions, password_hasher::PasswordHandlerExtensions
};

// Generate tokens for a user
let tokens = auth_service.generate_tokens("user_id")?;

// Or use the trait extensions
let tokens = user.generate_tokens(auth_service)?;

// Custom token with additional claims
let token = auth_service.generate_token(
    TokenOptions::new("user_id", TokenPurpose::Access)
        .with_additional_claims(MyClaims { role: "admin".into() })?
)?;
```

### Password Hashing

Argon2-based password hashing with trait extensions:

```rust
use web_core::auth::password_hasher::{PasswordHandler, PasswordHandlerExtensions};

#[derive(Clone)]
struct User {
    password_hash: String,
}

impl PasswordHandler for User {
    fn get_password_hash(&self) -> Result<String, Error> {
        Ok(self.password_hash.clone())
    }

    fn set_password_hash(&mut self, hash: String) {
        self.password_hash = hash;
    }
}

// Update password
user.update_password("new_password")?;

// Validate password
if user.validate_password("password".into())? {
    // Password is valid
}
```

### Google OAuth

```rust
use web_core::auth::google::verify_google_token;

let email = verify_google_token(client_id, id_token).await?;
```

### Ed25519 Signing

```rust
use web_core::utils::signatory;

// Sign data
let signature = signatory::sign(&signing_key_hex, "data to sign")?;

// Verify signature
signatory::verify(&verifying_key_hex, "data to sign", &signature)?;
```

### Error Handling

Structured error types with automatic conversion:

```rust
use web_core::{
    error::Error,
    bad_request, unauthorized, not_found, something_went_wrong
};

// Return errors from handlers
fn my_handler() -> Result<(), Error> {
    // Bad request with field validation
    return Err(bad_request!("Invalid email", {"email": "malformed"}));

    // Simple bad request
    return Err(bad_request!("Something went wrong"));

    // Unauthorized
    return Err(unauthorized!("Token expired"));

    // Not found
    return Err(not_found!("User not found"));

    // Internal error (logged but generic message to client)
    return Err(something_went_wrong!("Database connection failed: {}", err));
}
```


### Testing Utilities

Test helpers for making API requests:

```rust
use web_core::test::router_extensions::RouterExtensions;

// In tests
let response: MyResponse = app
    .post_api("/api/users", Some(request_body))
    .await?;

// With authentication
let response: MyResponse = app
    .post_api_with_access_token("/api/posts", access_token, Some(request_body))
    .await?;

// GET requests
let response: MyResponse = app
    .get_api_with_access_token("/api/posts/123", access_token)
    .await?;

// PUT requests
let response: MyResponse = app
    .put_api("/api/users/123", access_token, request_body)
    .await?;

// DELETE requests
app.delete_api("/api/posts/123", access_token).await?;
```

### Diesel Integration (feature: `diesel`)

JSONB support for PostgreSQL:

```rust
use web_core::diesel::jsonb_data::JsonbData;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserProfile {
    name: String,
    preferences: Vec<String>,
}

// In your Diesel model
table! {
    users (id) {
        id -> Int4,
        profile -> Jsonb,
    }
}

#[derive(Queryable, Insertable)]
struct User {
    id: i32,
    profile: JsonbData<UserProfile>,
}
```

### AWS Integration (feature: `aws`)

```rust
use web_core::aws::mail::send_email;

// Send email via AWS SES
send_email(
    &ses_client,
    "sender@example.com",
    "recipient@example.com",
    "Subject",
    "HTML body",
    "Text body"
).await?;
```

## Modules

| Module | Description |
|--------|-------------|
| `auth` | JWT authentication, password hashing, Google OAuth |
| `cors` | CORS configuration with automatic www/non-www handling |
| `error` | Error types and response handling |
| `middleware` | Logging and custom middleware support |
| `validators` | Password, phone number, and request validation |
| `utils` | Ed25519 signing utilities |
| `test` | Testing helpers for router testing |
| `aws` *(feature)* | AWS SES and CloudFront utilities |
| `diesel` *(feature)* | Diesel ORM integration with JSONB |
| `web_core` | Core types: `WebCoreState`, `WebCoreOptions`, router extensions |

## Error Types

All errors implement `IntoResponse` for use in Axum handlers:

- `FieldValidationError` → 400 Bad Request (field-level errors)
- `BadRequestError` → 400 Bad Request (general errors)
- `AuthenticationFailure` → 401 Unauthorized
- `NotFound` → 404 Not Found
- `SomethingWentWrong` → 500 Internal Server Error (logs details, returns generic message)

Validator crates are a fork of Salman-Sali/validator-async which is a fork of Keats/validator. Validator crates contain extra logic to inject state.

## License

This project is licensed under the terms specified in the LICENSE file.
