use std::time::Duration;

use diesel_async::{
    AsyncPgConnection,
    pooled_connection::{AsyncDieselConnectionManager, deadpool::Pool},
};
use diesel_migrations::EmbeddedMigrations;

use crate::{error::Error, something_went_wrong};

/// Async Diesel connection pool (deadpool) for Postgres.
pub type AsyncPgPool = diesel_async::pooled_connection::deadpool::Pool<AsyncPgConnection>;

/// Async counterpart of [`create_pg_pool`](super::create_pg_pool).
///
/// Creates the database named in `database_url` (with sqlx) when it is missing, runs the pending
/// embedded `migrations` on a sync `diesel::PgConnection` inside `tokio::task::spawn_blocking`,
/// and returns a deadpool pool of `AsyncPgConnection`s holding at most `max_size` connections.
///
/// ```rust,ignore
/// use diesel_migrations::{EmbeddedMigrations, embed_migrations};
/// use web_core::diesel::{AsyncPgPool, create_async_pg_pool};
///
/// const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
///
/// let pool: AsyncPgPool = create_async_pg_pool(MIGRATIONS, &database_url, 16).await?;
/// let mut conn = pool.get().await?;
/// ```
pub async fn create_async_pg_pool(
    migrations: EmbeddedMigrations,
    database_url: &str,
    max_size: usize,
) -> Result<AsyncPgPool, Error> {
    super::ensure_database_exists(database_url).await?;

    let url = database_url.to_owned();
    tokio::task::spawn_blocking(move || super::run_migrations_blocking(&url, migrations))
        .await
        .map_err(|e| something_went_wrong!("Migration task failed: {:?}", e))??;

    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder(manager)
        .max_size(max_size)
        .build()
        .map_err(|e| something_went_wrong!("Error while building async pg pool: {:?}", e))
}

/// Settings for [`create_async_pg_pool_with_options`].
///
/// `wait_timeout` bounds how long `pool.get()` waits for a free slot, `create_timeout` how long
/// opening a new connection may take and `recycle_timeout` how long the health check of a reused
/// connection may take. `None` means no limit (deadpool's default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPgPoolOptions {
    pub max_size: usize,
    pub wait_timeout: Option<Duration>,
    pub create_timeout: Option<Duration>,
    pub recycle_timeout: Option<Duration>,
}

impl AsyncPgPoolOptions {
    /// `max_size` connections at most, no timeouts.
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            wait_timeout: None,
            create_timeout: None,
            recycle_timeout: None,
        }
    }

    pub fn with_wait_timeout(mut self, timeout: Duration) -> Self {
        self.wait_timeout = Some(timeout);
        self
    }

    pub fn with_create_timeout(mut self, timeout: Duration) -> Self {
        self.create_timeout = Some(timeout);
        self
    }

    pub fn with_recycle_timeout(mut self, timeout: Duration) -> Self {
        self.recycle_timeout = Some(timeout);
        self
    }
}

/// Builds a pool for an **existing** database without creating it or running migrations, e.g.
/// a lazily created per-tenant pool. Same as
/// [`create_async_pg_pool_with_options`]`(database_url, AsyncPgPoolOptions::new(max_size))`.
///
/// The pool is lazy: no connection is opened until the first `pool.get()`. If the database does
/// not exist, that first `get()` fails (and nothing is created).
pub fn create_async_pg_pool_without_migrations(
    database_url: &str,
    max_size: usize,
) -> Result<AsyncPgPool, Error> {
    create_async_pg_pool_with_options(database_url, AsyncPgPoolOptions::new(max_size))
}

/// Builds a lazy pool for an existing database (no database creation, no migrations) using
/// `options`. Timeouts run on the Tokio runtime.
///
/// ```rust,ignore
/// use std::time::Duration;
/// use web_core::diesel::{AsyncPgPoolOptions, create_async_pg_pool_with_options};
///
/// let pool = create_async_pg_pool_with_options(
///     &tenant_database_url,
///     AsyncPgPoolOptions::new(3).with_wait_timeout(Duration::from_secs(5)),
/// )?;
/// ```
pub fn create_async_pg_pool_with_options(
    database_url: &str,
    options: AsyncPgPoolOptions,
) -> Result<AsyncPgPool, Error> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    Pool::builder(manager)
        .max_size(options.max_size)
        .wait_timeout(options.wait_timeout)
        .create_timeout(options.create_timeout)
        .recycle_timeout(options.recycle_timeout)
        .runtime(deadpool::Runtime::Tokio1)
        .build()
        .map_err(|e| something_went_wrong!("Error while building async pg pool: {:?}", e))
}

/// Runs the pending embedded `migrations` against the (existing) database at `database_url`
/// on a blocking thread. Does not create the database. Use it for an explicit migrate step
/// (e.g. a `migrate-all` command) next to [`create_async_pg_pool_without_migrations`].
pub async fn run_pending_migrations(
    migrations: EmbeddedMigrations,
    database_url: &str,
) -> Result<(), Error> {
    let url = database_url.to_owned();
    tokio::task::spawn_blocking(move || super::run_migrations_blocking(&url, migrations))
        .await
        .map_err(|e| something_went_wrong!("Migration task failed: {:?}", e))?
}

/// Closes the idle connections of `pool` that were last checked out more than `max_idle` ago
/// and returns how many were closed. Connections currently in use are never touched.
///
/// deadpool has no idle timeout of its own; call this from one app-wide interval over all live
/// pools (it only borrows the pool, so it never keeps a pool alive):
///
/// ```rust,ignore
/// let mut tick = tokio::time::interval(Duration::from_secs(30));
/// loop {
///     tick.tick().await;
///     for (_, tenant) in tenant_cache.iter() {
///         web_core::diesel::retain_idle(&tenant.pool, Duration::from_secs(60));
///     }
/// }
/// ```
///
/// deadpool measures "last used" from the moment a connection was last handed out by
/// `pool.get()`, not from when it was returned, so a connection held longer than `max_idle`
/// is closed on the first sweep after it is returned.
pub fn retain_idle(pool: &AsyncPgPool, max_idle: Duration) -> usize {
    pool.retain(|_, metrics| metrics.last_used() < max_idle)
        .removed
        .len()
}
