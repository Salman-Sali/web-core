//! The app side (feature `app`): [`RunnerPool`] with its scheduler, leases and axum routes,
//! the [`RunnerStore`] the app implements for persistence, admin DTOs, the local fallback
//! supervisor and (with feature `runner` too) the in-process test runner.
//!
//! ```no_run
//! use std::sync::Arc;
//! use serde_json::json;
//! use web_core_runners::app::{MemoryStore, PoolConfig, RunnerPool, SubmitOptions, TaskFiles};
//!
//! # async fn example() -> Result<(), web_core::error::Error> {
//! let pool = RunnerPool::start(
//!     PoolConfig::new("<contract hash>", "https://api.example.com"),
//!     Arc::new(MemoryStore::new()), // the app's Diesel-backed store in production
//! )
//! .await?;
//! let app: axum::Router = axum::Router::new().merge(pool.router());
//!
//! let handle = pool
//!     .submit(
//!         "thumbnail",
//!         json!({"width": 320}),
//!         TaskFiles::new().input("image", "/data/a.png").output("thumbnail", "/data/a.thumb.png"),
//!         SubmitOptions::new().with_reference("job-42"),
//!     )
//!     .await?;
//! let mut progress = handle.progress();
//! let output = handle.await; // Result<TaskOutput, TaskError>
//! # Ok(()) }
//! ```

mod admin;
mod handle;
mod local;
mod pool;
mod routes;
mod store;

#[cfg(feature = "runner")]
mod in_process;

pub use admin::{
    AttemptView, RegisterRunnerRequest, RegisterRunnerResponse, RunnerSettingsPatch, RunnerView,
    TaskHistoryView,
};
pub use handle::{
    InputSource, OutputFile, OutputTarget, RetryHook, SubmitOptions, TaskCanceller, TaskError,
    TaskFiles, TaskHandle, TaskOutput, TaskProgress, TypedOutput, TypedTaskHandle,
};
#[cfg(feature = "runner")]
pub use in_process::{InProcessRunner, InProcessRunnerBuilder, InProcessSpawner};
pub use local::{
    LocalCommand, LocalFallbackConfig, LocalFallbackState, LocalFallbackSupervisor,
    LocalProcess, LocalRunnerMode, LocalRunnerStatus, ProcessSpawner, TokioSpawner,
};
pub use pool::{PoolConfig, PoolSummary, RunnerPool, RunnerSession, issue_runner_token};
pub use store::{
    AttemptRecord, AttemptStatus, MemoryStore, RunnerRecord, RunnerSettings, RunnerState,
    RunnerStatus, RunnerStore, TaskRecord, TaskStatus,
};
