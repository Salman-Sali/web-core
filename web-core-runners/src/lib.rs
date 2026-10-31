//! Remote task runners for web-core apps.
//!
//! An app hands pure units of work (**tasks**: input files + a JSON input in, output files + a
//! JSON output out) to a pool of **runners**, separate binaries on other machines that dial out
//! to the app over a WebSocket. Files travel over HTTPS with signed, expiring URLs.
//!
//! - [`protocol`] (always available): the wire messages, the [`protocol::Task`] trait and the
//!   contract fingerprint helpers. A product's contract crate depends on this part only.
//! - [`runner`] (feature `runner`): the runner runtime. A product's runner binary is a
//!   [`runner::TaskRegistry`] plus one call to [`runner::main`].
//! - [`app`] (feature `app`): the app-side [`app::RunnerPool`] (scheduler, leases, axum routes,
//!   admin operations), the local fallback supervisor and, with both features, the
//!   in-process test runner.
//!
//! See `docs/runners-protocol.md` for the message-level spec and the README for the tables an
//! app is expected to create.

// `async_trait` expands to `#[must_use]` on boxed futures, which clippy flags.
#![allow(clippy::double_must_use)]

pub mod protocol;

#[cfg(any(feature = "app", feature = "runner"))]
pub mod hash;

#[cfg(feature = "runner")]
pub mod runner;

#[cfg(feature = "app")]
pub mod app;

pub use protocol::{PROTOCOL_VERSION, Task};
