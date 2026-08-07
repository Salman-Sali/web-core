#![cfg(feature = "diesel")]

use crate::error::Error;
use diesel::{
    PgConnection,
    r2d2::{ConnectionManager, Pool},
};
use diesel_migrations::EmbeddedMigrations;
use sqlx::migrate::MigrateDatabase;

pub mod jsonb_data;

pub async fn create_pg_pool(
    migrations: EmbeddedMigrations,
    database_url: &str,
) -> Result<Pool<ConnectionManager<PgConnection>>, Error> {
    use diesel::Connection;
    use diesel_migrations::MigrationHarness;

    use crate::something_went_wrong;
    let database_exists = sqlx::Postgres::database_exists(database_url)
        .await
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    if !database_exists {
        sqlx::Postgres::create_database(database_url)
            .await
            .map_err(|e| something_went_wrong!("{:?}", e))?;
    }

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
