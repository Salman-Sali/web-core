# web-core

A Rust crate providing core utilities for building web applications with Axum. Features JWT authentication, password hashing, structured error handling, request validation, CORS, testing helpers, and more.

## Overview

`web-core` consolidates common web backend patterns into a cohesive toolkit, reducing boilerplate when building Axum-based APIs. It emphasizes trait-based extensibility, uniform error responses, and state-aware validation.

## Installation

```toml
[dependencies]
web-core = { version = "0.5.0", features = ["derive"] }

[features]
# Enables CORS allow-all for local development
local = ["web-core/local"]

[dev-dependencies]
web-core = { version = "0.5.0", features = ["all"] }
```

### Crate Structure

This workspace contains four crates:

| Crate | Description |
|-------|-------------|
| `web-core` | Main crate with auth, error handling, validation, and utilities |
| `web-core-derive` | Procedural macros (e.g., `diesel_jsonb`) |
| `web-core-validator` | Validation traits and implementations (fork of Keats/validator) |
| `web-core-validator-derive` | Procedural macros for `derive(Validate)` |
| `web-core-backup` | Postgres + folder backups to S3 with `backup`/`restore` subcommands ([README](web-core-backup/README.md)) |
| `web-core-runners` | Remote task runners: wire protocol, runner runtime (feature `runner`) and app-side pool (feature `app`) ([README](web-core-runners/README.md)) |

### Optional Features

| Feature | Description |
|---------|-------------|
| `aws` | AWS SES email sending |
| `diesel` | Diesel ORM integration (sync r2d2 pool, pagination, JSONB, `TestDatabase`) |
| `diesel-async` | Async Diesel pool (`diesel-async` + deadpool) and `AsyncPaginatedLoad`; enables `diesel` |
| `derive` | Procedural macros (includes `diesel_jsonb`) |
| `all` | Enables all features (`aws`, `derive`, `diesel`, `diesel-async`) |

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

JWT-based authentication with configurable token lifetimes and audience:

```rust
use web_core::auth::{
    AuthOptions, AuthService, TokenOptions, TokenPurpose,
    AuthHandler, AuthHandlerExtensions
};

// Initialize auth service
let auth_options = AuthOptions::new(
    secret_key,
    Duration::from_secs(3600),      // Access token TTL
    Duration::from_secs(2592000)    // Refresh token TTL
);
let auth_service = AuthService::new(auth_options);

// Generate tokens
let tokens = auth_service.generate_tokens("user_id")?;

// Or using trait extensions
let tokens = user.generate_tokens(&auth_service)?;

// Custom claims
let token = auth_service.generate_token(
    TokenOptions::new("user_id", TokenPurpose::Access)
        .with_additional_claims(MyClaims { role: "admin".into() })?
)?;

// Middleware layer
let protected = Router::new()
    .route("/admin", get(admin_handler))
    .with_auth_layer(auth_service);
```

### Password Hashing

Argon2-based password hashing with trait-based extensions:

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

// Hash and set password
user.update_password("new_secure_password")?;

// Validate password
if user.validate_password("input_password")? {
    // Authenticated
}
```

### Google OAuth

```rust
use web_core::auth::google::verify_google_token;

let email = verify_google_token(client_id, id_token).await?;
```

### Request Validation

State-aware validation using `ValidatedJson`:

```rust
use web_core::validators::validated_json::ValidatedJson;
use web_core::validators::validator_context::ValidatorContext;
use web_core_validator::Validate;

#[derive(Deserialize, Validate)]
struct CreateUserRequest {
    #[validate(email)]
    email: String,
    #[validate(length(min = 8))]
    password: String,
}

async fn create_user(
    State(state): State<AppState>,
    ValidatedJson(body, _ctx): ValidatedJson<CreateUserRequest>,
) -> Result<Json<User>, Error> {
    // Validation passed, body.email and body.password are valid
    Ok(Json(create_in_db(&state.db, body).await?))
}
```

Validation with context:

```rust
#[derive(Deserialize, Validate)]
#[validate(context = ValidatorContext<AppState>)]
struct UpdateProfileRequest {
    #[validate(nested(use_context))]
    profile: ProfileData,
}

#[derive(Deserialize, Validate)]
struct ProfileData {
    #[validate(custom(function = "check_unique_username"))]
    username: String,
}

// Custom validation function with access to state and request parts
fn check_unique_username(
    username: &str,
    ctx: &mut ValidatorContext<AppState>
) -> Result<(), ValidationError> {
    // Access application state
    let db = ctx.state().db.clone();

    // Access request parts (headers, extensions, etc.)
    let parts = ctx.parts();
    let request_id = parts.headers.get("x-request-id");

    // Extract path parameters
    let user_id: String = futures::executor::block_on(ctx.get_path_value())?;

    // Use memoization for expensive lookups
    if let Some(cached) = ctx.get::<bool>("username_checked") {
        return Ok(());
    }

    // Perform unique check
    let is_unique = check_db(&db, username)?;
    ctx.insert("username_checked", is_unique);

    Ok(())
}
```

The `ValidatorContext<S>` provides:
- `.state()` → reference to your application state
- `.parts()` → reference to HTTP request parts (headers, method, uri, extensions)
- `.get_path_value<T>()` → extract and deserialize path parameters
- `.from_request_parts::<T>()` → extract any `FromRequestParts` type (e.g., `TypedHeader`)
- `.insert(key, value)` / `.get(key)` / `.pop(key)` → memoization for expensive operations

### Error Handling

Structured errors with automatic `IntoResponse`:

```rust
use web_core::{
    error::Error,
    bad_request, unauthorized, not_found, forbidden, conflict, something_went_wrong
};

fn my_handler() -> Result<(), Error> {
    // Field validation errors
    Err(bad_request!("Invalid email", {"email": "not a valid email"}))

    // Simple messages
    Err(bad_request!("Missing required field"))
    Err(unauthorized!("Token expired or invalid"))
    Err(forbidden!("Insufficient permissions"))
    Err(not_found!("User not found"))

    // 409 Conflict: {"error": "Order 42 is already paid"}
    Err(conflict!("Order {} is already paid", 42))

    // Internal error (details logged, generic message to client)
    Err(something_went_wrong!("DB connection failed: {}", err))
}
```

With the `diesel` feature, `?` on a `diesel::result::Error` maps a unique-constraint violation to
`Error::Conflict` (409, `"Conflict on <constraint>"`, or `"Conflict"` when Postgres reports no
constraint name). Every other Diesel error stays a 500.

#### Internal errors from validators

A custom validator that cannot decide (for example its database lookup failed) should return
`ValidationError::internal(err)` instead of unwrapping or returning a misleading 400. When the
validation result of `ValidatedJson` / `ValidatedJsonWithCtx` contains such an error (anywhere,
including nested structs and lists), the request fails with a 500 (`something_went_wrong!`
carrying the error's message) instead of a 400 field-validation error:

```rust
use web_core_validator::ValidationError;

async fn unique_email(email: &str, ctx: &mut ValidatorContext<AppState>) -> Result<(), ValidationError> {
    let taken = email_exists(ctx.state(), email)
        .await
        .map_err(ValidationError::internal)?; // code "__internal__", message = err.to_string()
    if taken {
        return Err(("email_taken", "Email is already registered").into());
    }
    Ok(())
}
```

`ValidationError::is_internal()` tells such errors apart.

### Logging (tracing)

web-core logs through [`tracing`](https://docs.rs/tracing), so install a subscriber in your
binary (for example `tracing_subscriber::fmt::init()`), otherwise nothing is printed.

- The request logger (`with_logging_layer`) emits one `info` event per request with the fields
  `status`, `method`, `uri`, `client_ip` (`x-forwarded-for` or `unknown`) and `elapsed_ms`; the
  message keeps the old `[200 OK] GET /path from 1.2.3.4 in 3.2ms` shape.
- 500 responses (`SomethingWentWrong`) emit an `error` event with the fields `error_id` and
  `details`.

Error types automatically serialize to appropriate HTTP responses with consistent JSON structure.

### CORS Configuration

By default `with_web_core` allows the URLs given with `with_frontend_url` plus their www /
non-www twin (debug builds allow every origin). An app can decide origins itself instead:

```rust
use tower_http::cors::AllowOrigin;

// Dynamic check, e.g. a tenant host cache; keeps web-core's headers/methods.
let options = WebCoreOptions::new(web_core_state).with_allowed_origin(
    AllowOrigin::async_predicate(move |origin, _parts| {
        let hosts = hosts.clone();
        async move { hosts.is_known_origin(&origin).await }
    }),
);

// Or a complete custom layer (`web_core::cors::base_cors_layer()` is the default without origins).
let options = WebCoreOptions::new(web_core_state).with_cors_layer(my_cors_layer);
```

With a custom origin check or layer, `with_frontend_url` is not used for CORS and debug builds
do not allow every origin.

### Testing Utilities

Test helpers for making authenticated and unauthenticated requests:

```rust
use web_core::test::router_extensions::RouterExtensions;

// POST without auth
let response: CreateResponse = app
    .post_api("/api/users", Some(request_body))
    .await?;

// POST with access token
let response: PostResponse = app
    .post_api_with_access_token("/api/posts", access_token, Some(request_body))
    .await?;

// GET with auth
let response: GetResponse = app
    .get_api_with_access_token("/api/posts/123", access_token)
    .await?;

// PUT and DELETE
let response: UpdateResponse = app
    .put_api("/api/users/123", access_token, request_body)
    .await?;
app.delete_api("/api/posts/123", access_token).await?;
```

### Diesel Integration (feature: `diesel`)

Sync pool (r2d2) with automatic database creation and embedded migrations, and pagination:

```rust
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use web_core::diesel::{PaginatedLoad, create_pg_pool};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

let pool = create_pg_pool(MIGRATIONS, &database_url).await?;
let page: PaginatedResult<ProductDto> = products::table
    .select(Product::as_select())
    .paginated_load::<Product, ProductDto>(&params, &mut pool.get()?)?;
```

`create_pg_pool` uses r2d2's default size: at most 10 connections, all opened at startup. To
size the pool, use `create_pg_pool_sized` (same database creation and migrations) with a
`PgPoolSize`; `min_idle` is how many idle connections r2d2 keeps open (and opens up front),
`None` meaning `max_size`:

```rust
use web_core::diesel::{PgPoolSize, create_pg_pool_sized};

// At most 4 connections, only 1 opened at startup.
let pool = create_pg_pool_sized(MIGRATIONS, &database_url, PgPoolSize::new(4).with_min_idle(1)).await?;
```

### Async Diesel (feature: `diesel-async`)

`create_async_pg_pool` behaves like `create_pg_pool` (creates the database if missing, runs the
pending embedded migrations on a sync connection inside `spawn_blocking`) and returns a
deadpool pool of `diesel_async::AsyncPgConnection`s:

```rust
use web_core::diesel::{AsyncPaginatedLoad, AsyncPgPool, create_async_pg_pool};

let pool: AsyncPgPool = create_async_pg_pool(MIGRATIONS, &database_url, 16).await?;
let mut conn = pool.get().await?; // deadpool PoolError converts into web_core::error::Error

let page: PaginatedResult<ProductDto> = products::table
    .select(Product::as_select())
    .order(products::id)
    .paginated_load::<Product, ProductDto>(&params, &mut conn)
    .await?;
```

#### Pools for existing databases (e.g. per tenant)

`create_async_pg_pool_without_migrations` and `create_async_pg_pool_with_options` neither create
the database nor run migrations, and open no connection until the first `pool.get()` (a missing
database makes that `get()` fail). Migrations run as an explicit step with
`run_pending_migrations`. deadpool has no idle timeout; `retain_idle` closes idle connections
and is meant to be called from one app-wide interval. Dropping the last pool handle closes its
idle connections (checked-out ones close when released).

```rust
use web_core::diesel::{
    AsyncPgPoolOptions, create_async_pg_pool_with_options, retain_idle, run_pending_migrations,
};

let pool = create_async_pg_pool_with_options(
    &tenant_url,
    AsyncPgPoolOptions::new(3).with_wait_timeout(Duration::from_secs(5)),
)?;

// migrate-all
run_pending_migrations(MIGRATIONS, &tenant_url).await?;

// every 30 s, over all cached pools
let closed: usize = retain_idle(&pool, Duration::from_secs(60));
```

`AsyncPaginatedLoad` runs the same SQL as `PaginatedLoad`
(`SELECT *, COUNT(*) OVER () FROM (...) t LIMIT $1 OFFSET $2`) and its future is `Send`.
Import either `PaginatedLoad` or `AsyncPaginatedLoad` in a module, not both: both provide
`paginated_load` for every query, so having both in scope is ambiguous.

### Per-test databases (feature: `diesel`)

`web_core::test::TestDatabase` gives every test its own migrated Postgres database:

```rust
use web_core::test::TestDatabase;

#[tokio::test]
async fn creates_order() {
    let db = TestDatabase::create(&std::env::var("TEST_DATABASE_URL").unwrap(), MIGRATIONS)
        .await
        .unwrap();
    let pool = create_async_pg_pool(MIGRATIONS, &db.url, 4).await.unwrap();
    // ... test against `pool` ...
} // dropping `db` runs DROP DATABASE ... WITH (FORCE)
```

- `base_url` is any database URL on the target server; its database name is only used as a
  prefix (`<name>_tpl_<hash>` for the template, `<name>_<random>` for test databases).
- Once per process and migration set, a template database is created and migrated (serialised
  across processes with a Postgres advisory lock); templates of older migration sets are dropped
  when unused. Each `create` then clones the template with `CREATE DATABASE ... TEMPLATE`, which
  is much faster than migrating per test.
- The template hash covers the migration *names*: rename or add a migration (as diesel does for
  new ones) to get a fresh template; editing an existing migration in place keeps the old one.
- Needs Postgres 13+ (`DROP DATABASE ... WITH (FORCE)`) and a role allowed to create databases.

#### JSONB

JSONB support for PostgreSQL with automatic serialization:

```rust
use web_core::diesel::jsonb_data::JsonbData;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserProfile {
    name: String,
    preferences: Vec<String>,
}

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

Derive macro for JSONB columns:

```rust
use web_core::diesel_jsonb;

#[derive(Serialize, Deserialize, diesel_jsonb)]
struct Settings {
    theme: String,
    notifications: bool,
}
```

### Ed25519 Signing

```rust
use web_core::utils::signatory;

// Sign data
let signature = signatory::sign(&signing_key_hex, "data to sign")?;

// Verify signature
signatory::verify(&verifying_key_hex, "data to sign", &signature)?;

// Generate new keypair
let (sk, vk) = signatory::generate_keypair()?;
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
    "<h1>HTML Body</h1>",
    "Plain text body"
).await?;
```

## Remote Task Runners (`web-core-runners`)

`web-core-runners` (0.5.0) moves CPU-heavy, pure units of work (**tasks**) out of the API onto
**runners**: separate binaries on other machines that dial out to the app over a WebSocket.
Files go over HTTPS with signed, expiring, resumable URLs; orchestration and the database stay
in the app.

| Feature | For | Main items |
|---------|-----|------------|
| (none) | the product's contract crate | `protocol::Task`, `protocol::Contract` (contract hash), the wire messages |
| `runner` | the product's runner binary | `runner::TaskRegistry`, `runner::main`, `TaskContext` |
| `app` | the API | `app::RunnerPool` (`submit`, `submit_typed`, admin operations, `router()`), `app::RunnerStore`, `app::LocalFallbackSupervisor` |
| `app` + `runner` | app tests | `app::InProcessRunner`, `app::InProcessSpawner` |

```toml
[dependencies]
web-core-runners = { version = "0.5.0", features = ["app"] }    # API
# web-core-runners = { version = "0.5.0", features = ["runner"] } # runner binary
```

The crate owns no tables: the app implements `RunnerStore` over its own `runners`,
`runner_tasks` and `runner_attempts` tables. See the [crate README](web-core-runners/README.md)
for usage and the table shapes, [docs/runners-protocol.md](docs/runners-protocol.md) for the
wire protocol and [docs/runners-plan.md](docs/runners-plan.md) for the design.

## Modules

| Module | Description |
|--------|-------------|
| `auth` | JWT authentication, password hashing, Google OAuth |
| `cors` | CORS configuration with automatic www/non-www handling |
| `error` | Error types with `IntoResponse` implementations |
| `middleware` | Logging and custom middleware support |
| `validators` | `ValidatedJson`, password/phone validators, validation context |
| `utils` | Ed25519 signing utilities |
| `test` | Router testing helpers, `TestDatabase` *(feature `diesel`)* |
| `pagination` | Pagination utilities |
| `web_core` | Core types: `WebCoreState`, `WebCoreOptions`, router extensions |
| `axum` | Axum-specific utilities |
| `diesel` | Diesel ORM integration: sync/async pools, pagination, JSONB support |
| `reqwest` | Reqwest client utilities |
| `aws` *(feature)* | AWS SES email utilities |

## Error Types

All errors implement `IntoResponse` for seamless use in Axum handlers:

| Error Type | HTTP Status | Description |
|------------|-------------|-------------|
| `FieldValidationError` | 400 | Field-level validation failures |
| `BadRequestError` | 400 | General bad request |
| `AuthenticationFailure` | 401 | Invalid or missing credentials |
| `NotFound` | 404 | Resource not found |
| `Conflict` | 409 | Conflicting state, e.g. unique-constraint violation (`conflict!`) |
| `Forbidden` | 403 | Permission denied |
| `SomethingWentWrong` | 500 | Internal server error (logs details, returns generic message) |

## Validation Crate Notes

The validator crates (`web-core-validator`, `web-core-validator-derive`) are based on Salman-Sali/validator-async, which is a fork of Keats/validator. Key differences from the original:

- **State-aware validation**: `Validate` trait accepts state parameter `S: Send + Sync`
- **Mutable context**: `ValidateArgs` uses `&mut Self::Args` for context propagation
- **Nested context**: `#[validate(nested(use_context))]` propagates parent context to nested validations

## License

MIT License - see [LICENSE](LICENSE) file for details.
