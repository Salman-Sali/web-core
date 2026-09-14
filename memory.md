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
| `src/validators/` | Password, phone, validated_json extractors, validator context |
| `src/test/` | Router testing utilities |
| `src/macros.rs` | Error macros (bad_request!, unauthorized!, etc) |
| `src/pagination/` | Pagination utilities |
| `src/axum/` | Axum-specific utilities |
| `src/reqwest/` | Reqwest client helpers |
| `web-core-derive/` | Procedural macros (diesel_jsonb) |
| `web-core-validator/` | Validation traits and implementations (state-aware) |
| `web-core-validator-derive/` | Procedural macros for derive(Validate) |

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
- `FieldErrors` is a tuple struct wrapping `HashMap<String, FieldError>`
- Custom `Serialize` impl for `FieldErrors` forwards to inner HashMap to avoid extra nesting in serialized output

## ValidatorContext Notes
- `from_request_parts` requires `&mut self` because `FromRequestParts::from_request_parts` needs `&mut Parts`
- Returns `Result<T, T::Rejection>` to properly propagate extraction errors
