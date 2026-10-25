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
