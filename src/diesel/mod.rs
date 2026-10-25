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
pub use async_pool::{AsyncPgPool, create_async_pg_pool};
pub use paginated_load::PaginatedLoad;

pub async fn create_pg_pool(
    migrations: EmbeddedMigrations,
    database_url: &str,
) -> Result<Pool<ConnectionManager<PgConnection>>, Error> {
    use diesel::Connection;
    use diesel_migrations::MigrationHarness;

    use crate::something_went_wrong;
    ensure_database_exists(database_url).await?;

    let mut setup_conn =
        PgConnection::establish(database_url).map_err(|e| something_went_wrong!("{:?}", e))?;
    setup_conn
        .run_pending_migrations(migrations)
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    drop(setup_conn);

    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let pool_builder = diesel::r2d2::Pool::builder();
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
