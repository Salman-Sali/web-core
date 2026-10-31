#![cfg(feature = "diesel")]

use crate::error::Error;
use diesel::{
    PgConnection,
    r2d2::{ConnectionManager, Pool},
};
use diesel_migrations::EmbeddedMigrations;
use sqlx::migrate::MigrateDatabase;

#[cfg(feature = "diesel-async")]
mod async_paginated_load;
#[cfg(feature = "diesel-async")]
mod async_pool;
pub mod jsonb_data;
mod paginated_load;

#[cfg(feature = "diesel-async")]
pub use async_paginated_load::AsyncPaginatedLoad;
#[cfg(feature = "diesel-async")]
pub use async_pool::{
    AsyncPgPool, AsyncPgPoolOptions, create_async_pg_pool, create_async_pg_pool_with_options,
    create_async_pg_pool_without_migrations, retain_idle, run_pending_migrations,
};
pub use paginated_load::PaginatedLoad;

/// Creates the database named in `database_url` (with sqlx) when it is missing, runs the pending
/// embedded `migrations` and returns an r2d2 pool of `PgConnection`s with r2d2's default size
/// (at most 10 connections, all opened up front).
///
/// Same as [`create_pg_pool_sized`]`(migrations, database_url, PgPoolSize::default())`; use that
/// to choose the pool size.
pub async fn create_pg_pool(
    migrations: EmbeddedMigrations,
    database_url: &str,
) -> Result<Pool<ConnectionManager<PgConnection>>, Error> {
    create_pg_pool_sized(migrations, database_url, PgPoolSize::default()).await
}

/// Size of the r2d2 pool built by [`create_pg_pool_sized`].
///
/// `max_size` is the most connections the pool holds. `min_idle` is how many idle connections
/// r2d2 keeps open (and opens before the pool is returned); `None` means r2d2's default of
/// `max_size`, i.e. every connection is opened at startup.
///
/// `PgPoolSize::default()` is r2d2's default: `max_size` 10, `min_idle` `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgPoolSize {
    pub max_size: u32,
    pub min_idle: Option<u32>,
}

impl PgPoolSize {
    /// `max_size` connections at most, r2d2's default `min_idle` (`max_size`).
    pub fn new(max_size: u32) -> Self {
        Self {
            max_size,
            min_idle: None,
        }
    }

    /// Keeps at most `min_idle` idle connections open instead of `max_size`, e.g. `0` to open
    /// connections only on demand.
    pub fn with_min_idle(mut self, min_idle: u32) -> Self {
        self.min_idle = Some(min_idle);
        self
    }
}

impl Default for PgPoolSize {
    fn default() -> Self {
        Self::new(10)
    }
}

/// [`create_pg_pool`] with a caller-chosen pool `size`: creates the database named in
/// `database_url` (with sqlx) when it is missing, runs the pending embedded `migrations` and
/// returns an r2d2 pool of `PgConnection`s.
///
/// Returns an error (before touching the database) when `size.max_size` is 0 or
/// `size.min_idle` is larger than `size.max_size`.
///
/// ```rust,ignore
/// use diesel_migrations::{EmbeddedMigrations, embed_migrations};
/// use web_core::diesel::{PgPoolSize, create_pg_pool_sized};
///
/// const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");
///
/// // At most 4 connections, keep at most 1 idle one.
/// let pool = create_pg_pool_sized(MIGRATIONS, &database_url, PgPoolSize::new(4).with_min_idle(1)).await?;
/// ```
pub async fn create_pg_pool_sized(
    migrations: EmbeddedMigrations,
    database_url: &str,
    size: PgPoolSize,
) -> Result<Pool<ConnectionManager<PgConnection>>, Error> {
    use diesel::Connection;
    use diesel_migrations::MigrationHarness;

    use crate::something_went_wrong;
    if size.max_size == 0 {
        return Err(something_went_wrong!("Pool max_size must be positive"));
    }
    if let Some(min_idle) = size.min_idle
        && min_idle > size.max_size
    {
        return Err(something_went_wrong!(
            "Pool min_idle ({}) must be no larger than max_size ({})",
            min_idle,
            size.max_size
        ));
    }

    ensure_database_exists(database_url).await?;

    let mut setup_conn =
        PgConnection::establish(database_url).map_err(|e| something_went_wrong!("{:?}", e))?;
    setup_conn
        .run_pending_migrations(migrations)
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    drop(setup_conn);

    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let pool_builder = diesel::r2d2::Pool::builder()
        .max_size(size.max_size)
        .min_idle(size.min_idle);
    Ok(pool_builder.build(manager)?)
}

/// Creates the database named in `database_url` (using sqlx) when it does not exist yet.
pub(crate) async fn ensure_database_exists(database_url: &str) -> Result<(), Error> {
    use crate::something_went_wrong;
    let database_exists = sqlx::Postgres::database_exists(database_url)
        .await
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    if !database_exists {
        sqlx::Postgres::create_database(database_url)
            .await
            .map_err(|e| something_went_wrong!("{:?}", e))?;
    }
    Ok(())
}

/// Opens a sync `PgConnection` to `database_url` and runs all pending embedded migrations.
/// Blocking: call it from `tokio::task::spawn_blocking` in async code.
pub(crate) fn run_migrations_blocking(
    database_url: &str,
    migrations: EmbeddedMigrations,
) -> Result<(), Error> {
    use diesel::Connection;
    use diesel_migrations::MigrationHarness;

    use crate::something_went_wrong;
    let mut conn =
        PgConnection::establish(database_url).map_err(|e| something_went_wrong!("{:?}", e))?;
    conn.run_pending_migrations(migrations)
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    Ok(())
}
