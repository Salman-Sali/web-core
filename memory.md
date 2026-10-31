# Memory

## Project Overview
`web-core` is a Rust crate providing core utilities for building web applications with Axum. It covers authentication, error handling, CORS, testing helpers, and more.

## Tech Stack
- Rust 2024 edition
- Axum 0.8.4
- JWT (jsonwebtoken)
- Argon2 for password hashing
- Ed25519 for signing
- Diesel ORM (optional; sync r2d2 via `diesel`, async deadpool via `diesel-async`)
- tracing for logs (request logger, 500 handler, reqwest helper)
- AWS SDK (optional)

## Architecture
- Feature-gated modules (`aws`, `diesel`, `diesel-async`, `derive`)
- Trait-based extensibility (`AuthHandler`, `PasswordHandler`, `WebCore`)
- Uniform error handling with `IntoResponse` impls
- CORS: default from `with_frontend_url` (+ www twin, Any in debug); `WebCoreOptions::with_allowed_origin(AllowOrigin)` / `with_cors_layer(CorsLayer)` override it (`cors::base_cors_layer()` = headers/methods, no origins)
- JWT-based auth with HS512 signing

## Key Locations
| Path | Purpose |
|------|---------|
| `src/auth/` | JWT, password hashing, Google OAuth |
| `src/error/` | Error types and IntoResponse impls |
| `src/web_core.rs` | WebCoreState, WebCoreOptions, router extensions |
| `src/validators/` | Password, phone, validated_json extractors, validator context |
| `src/test/` | Router testing utilities; `test_database.rs` = `TestDatabase` (per-test Postgres DB cloned from a migrated template) |
| `src/diesel/` | `create_pg_pool` + `PaginatedLoad` (sync), `async_pool.rs` + `async_paginated_load.rs` (feature `diesel-async`), JSONB |
| `tests/` | Workspace integration tests; `postgres.rs` needs `TEST_DATABASE_URL` (skips without it), migrations in `tests/migrations/` |
| `src/macros.rs` | Error macros (bad_request!, unauthorized!, not_found!, conflict!, something_went_wrong!) |
| `src/pagination/` | Pagination utilities |
| `src/axum/` | Axum-specific utilities |
| `src/reqwest/` | Reqwest client helpers |
| `web-core-derive/` | Procedural macros (diesel_jsonb) |
| `web-core-validator/` | Validation traits and implementations (state-aware) |
| `web-core-validator-derive/` | Procedural macros for derive(Validate) |
| `web-core-backup/` | Postgres + folder backups to S3, `<bin> backup` / `<bin> restore` subcommands, scheduler |

## Documentation
| File | Description |
|------|-------------|
| `README.md` | Main crate documentation with examples and module overview |

## User Preferences
- Do not fix errors unless explicitly asked
- Do not check for errors on added code
- No explanations unless asked

## Validator Crates Notes
- Fixed `mismatched_lifetime_syntaxes` warnings in `email.rs`, `regex.rs`, and `urls.rs`
- Changed `Cow<str>` to `Cow<'_, str>` and `Cow<Regex>` to `Cow<'_, Regex>` for consistency with Rust's elided lifetime syntax rules
- Added `S: Send + Sync` state parameter to `Validate` trait and all implementations (`macro_rules! impl_validate_list`, `[T; N]`, `HashMap`, `BTreeMap`)
- Updated `ValidateArgs` trait to take `args: &mut Self::Args` instead of `args: Self::Args`
- Updated derive macro to generate `Validate` and `ValidateArgs<'v_a>` implementations with mutable context forwarding to nested validations
- Removed `mutable` attribute from derive macro - context is now always mutable
- Fixed derive macro to generate `type Args = Context` instead of `type Args = &'v_a mut Context` - the associated type should be the bare type without reference since the function parameter is `&mut Self::Args`
- Added `From<(&str, &str)>` implementation for `ValidationError` to easily create errors with code and message
- Added `nested(use_context)` support to the validator derive macro - allows propagating parent context to nested struct validations via `validate_with_args`
  - New syntax: `#[validate(nested(use_context))]` on fields where the nested struct implements `ValidateArgs`
  - Works with `Option<T>` and `Vec<T>` nested types
  - Produces clear compile-time error if used without parent `#[validate(context = ...)]`
  - Added `is_option()` and `is_vec()` helper methods to `ValidateField`
  - Added custom `FromMeta` implementation for `Nested` struct to support both `#[validate(nested)]` and `#[validate(nested(use_context))]` syntaxes

## Error Handling Notes
- `Error::Conflict` (409, `{"error": msg}`) via `conflict!`; diesel `UniqueViolation` maps to it ("Conflict on <constraint>")
- `ValidationError::internal(e)` (code `__internal__`) anywhere in `ValidationErrors` makes `From<ValidationErrors> for Error` return 500 instead of 400
- `SomethingWentWrong::print_error` logs with `tracing::error!(error_id, details)`
- `FieldErrors` is a tuple struct wrapping `HashMap<String, FieldError>`
- Custom `Serialize` impl for `FieldErrors` forwards to inner HashMap to avoid extra nesting in serialized output

## Async Diesel / TestDatabase Notes
- `AsyncPaginatedLoad` reuses `Paginated<T>` (`Paginated::new`); diesel-async's `RunQueryDsl` is blanket-implemented for all types, so it is called by path to avoid shadowing `Vec::first`
- Projects import either `PaginatedLoad` or `AsyncPaginatedLoad`, not both (same method name)
- `TestDatabase` template name = `<base>_tpl_<8 hex FNV-1a of migration names>`; ready marker is a `COMMENT ON DATABASE`; Drop joins a std::thread running `DROP DATABASE ... WITH (FORCE)`
- sqlx needs `runtime-tokio` (enabled in web-core since 0.4.0) for the database-exists/create check
- Tenant pools (0.4.1): `create_async_pg_pool_without_migrations` / `create_async_pg_pool_with_options(url, AsyncPgPoolOptions)` never create the DB or migrate and are lazy; `run_pending_migrations` is the explicit step; `retain_idle(&pool, max_idle)` wraps `pool.retain` (deadpool `last_used` = time since last checkout). Direct `deadpool` dep only for `Runtime::Tokio1` (timeouts)
- Sized sync pool (0.4.2): `create_pg_pool_sized(migrations, url, PgPoolSize { max_size, min_idle })` (builder: `PgPoolSize::new(n).with_min_idle(k)`); `create_pg_pool` delegates with `PgPoolSize::default()` (r2d2 default: 10, min_idle None = all opened at startup). Invalid sizes (max 0, min_idle > max) return an error instead of r2d2's panic
- Postgres tests in `tests/postgres.rs` assert connection counts via `pg_stat_activity` (polling, backends exit async)
- `cargo --all-features` needs nightly (`web-core-validator/derive_nightly_features`)

## ValidatorContext Notes
- `from_request_parts` requires `&mut self` because `FromRequestParts::from_request_parts` needs `&mut Parts`
- Returns `Result<T, T::Rejection>` to properly propagate extraction errors
