# Memory

## Project Overview
`web-core` is a Rust crate providing core utilities for building web applications with Axum. It covers authentication, error handling, CORS, testing helpers, and more.

## Tech Stack
- Rust 2024 edition
- Axum 0.8.4
- JWT (jsonwebtoken)
- Argon2 for password hashing
- Ed25519 for signing
- Diesel ORM (optional)
- AWS SDK (optional)

## Architecture
- Feature-gated modules (`aws`, `diesel`, `derive`)
- Trait-based extensibility (`AuthHandler`, `PasswordHandler`, `WebCore`)
- Uniform error handling with `IntoResponse` impls
- JWT-based auth with HS512 signing

## Key Locations
| Path | Purpose |
|------|---------|
| `src/auth/` | JWT, password hashing, Google OAuth |
| `src/error/` | Error types and IntoResponse impls |
| `src/web_core.rs` | WebCoreState, WebCoreOptions, router extensions |
| `src/validators/` | Password, phone, validated_json extractors |
| `src/test/` | Router testing utilities |
| `src/macros.rs` | Error macros (bad_request!, unauthorized!, etc) |
| `web-core-derive/` | Procedural macros (diesel_jsonb) |
| `web-core-validator/` | Validation traits and implementations |
| `web-core-validator-derive/` | Procedural macros for derive(Validate) |

## User Preferences
- Do not fix errors unless explicitly asked
- Do not check for errors on added code
- No explanations unless asked

## Validator Crates Notes
- Fixed `mismatched_lifetime_syntaxes` warnings in `email.rs`, `regex.rs`, and `urls.rs`
- Changed `Cow<str>` to `Cow<'_, str>` and `Cow<Regex>` to `Cow<'_, Regex>` for consistency with Rust's elided lifetime syntax rules
- Added `S: Send + Sync` state parameter to `Validate` trait and all implementations (`macro_rules! impl_validate_list`, `[T; N]`, `HashMap`, `BTreeMap`)
- Updated `ValidateArgs` trait to also take `state: &S` parameter
- Updated derive macro to generate `Validate<S>` and `ValidateArgs<'v_a, S>` implementations with state forwarding to nested validations
- Added `From<(&str, &str)>` implementation for `ValidationError` to easily create errors with code and message
