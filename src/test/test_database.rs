//! Per-test Postgres databases cloned from a migrated template (feature `diesel`).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use diesel::migration::MigrationSource;
use diesel::{
    Connection, PgConnection, RunQueryDsl, connection::SimpleConnection, pg::Pg, sql_types::BigInt,
};
use diesel_migrations::EmbeddedMigrations;
use url::Url;

use crate::{error::Error, something_went_wrong};

/// Database every Postgres server has; used for CREATE / DROP DATABASE statements.
const MAINTENANCE_DATABASE: &str = "postgres";
/// `COMMENT ON DATABASE` marker written once the template is fully migrated.
const TEMPLATE_READY_MARKER: &str = "web-core TestDatabase template: ready";

/// A throw-away Postgres database for one test, cloned from a migrated template.
///
/// [`TestDatabase::create`] makes sure a template database
/// `<base_db_name>_tpl_<8 hex of the migration names>` exists and is migrated (once per process
/// and migration set, serialised across processes with a Postgres advisory lock), then clones
/// it with `CREATE DATABASE "<base_db_name>_<8 random hex>" TEMPLATE "<template>"`. Cloning is
/// much faster than running every migration per test, and every test gets its own isolated
/// database, so tests can run in parallel.
///
/// Dropping the guard drops the database (`DROP DATABASE ... WITH (FORCE)`, Postgres 13+) on a
/// separate OS thread; `drop` waits for that thread so the database is gone when the guard is.
/// Failures are logged with `tracing::warn!`, never panicked on.
///
/// Templates for older migration sets (`<base_db_name>_tpl_*` with another hash) are dropped
/// when they are not in use.
///
/// ```rust,ignore
/// use web_core::test::TestDatabase;
///
/// #[tokio::test]
/// async fn creates_user() {
///     let db = TestDatabase::create(&std::env::var("TEST_DATABASE_URL").unwrap(), MIGRATIONS)
///         .await
///         .unwrap();
///     let pool = create_async_pg_pool(MIGRATIONS, &db.url, 4).await.unwrap();
///     // ... run the test against `pool` ...
/// } // `db` dropped here -> database dropped
/// ```
pub struct TestDatabase {
    /// Connection URL of the new database (same server and credentials as `base_url`).
    pub url: String,
    /// Name of the new database.
    pub name: String,
    maintenance_url: String,
}

impl std::fmt::Debug for TestDatabase {
    // The URLs carry credentials, so they are left out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestDatabase")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl TestDatabase {
    /// `base_url`: any Postgres URL on the target server (its database name is used only as a
    /// prefix for the template and test database names; it is never connected to).
    pub async fn create(base_url: &str, migrations: EmbeddedMigrations) -> Result<Self, Error> {
        let base = Url::parse(base_url).map_err(|e| something_went_wrong!("Invalid url: {e}"))?;
        let base_name = match base.path().trim_start_matches('/') {
            "" => "web_core_test".to_string(),
            name => name.to_string(),
        };
        let maintenance_url = with_database(&base, MAINTENANCE_DATABASE);

        let hash = migrations_hash(&migrations)?;
        let template = format!("{base_name}_tpl_{:08x}", (hash >> 32) as u32);
        let template_url = with_database(&base, &template);

        let cell = {
            let mut templates = TEMPLATES
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            templates.entry(template.clone()).or_default().clone()
        };
        cell.get_or_try_init(|| {
            let maintenance_url = maintenance_url.clone();
            let template = template.clone();
            let template_prefix = format!("{base_name}_tpl_");
            async move {
                tokio::task::spawn_blocking(move || {
                    prepare_template(
                        &maintenance_url,
                        &template,
                        &template_url,
                        &template_prefix,
                        hash as i64,
                        migrations,
                    )
                })
                .await
                .map_err(|e| something_went_wrong!("Template task failed: {e:?}"))?
            }
        })
        .await?;

        let name = format!(
            "{base_name}_{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        {
            let maintenance_url = maintenance_url.clone();
            let name = name.clone();
            tokio::task::spawn_blocking(move || clone_template(&maintenance_url, &name, &template))
                .await
                .map_err(|e| something_went_wrong!("Create database task failed: {e:?}"))??;
        }

        Ok(Self {
            url: with_database(&base, &name),
            name,
            maintenance_url,
        })
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let maintenance_url = std::mem::take(&mut self.maintenance_url);
        let name = self.name.clone();
        let spawned = std::thread::Builder::new()
            .name("web-core-test-db-drop".into())
            .spawn(move || -> Result<(), String> {
                let mut conn =
                    PgConnection::establish(&maintenance_url).map_err(|e| e.to_string())?;
                conn.batch_execute(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    quote_ident(&name)
                ))
                .map_err(|e| e.to_string())
            });
        match spawned.map(|handle| handle.join()) {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => {
                tracing::warn!(database = %self.name, "Failed to drop test database: {e}")
            }
            Ok(Err(_)) => {
                tracing::warn!(database = %self.name, "Test database drop thread panicked")
            }
            Err(e) => {
                tracing::warn!(database = %self.name, "Failed to spawn test database drop thread: {e}")
            }
        }
    }
}

/// One init cell per template name, so each template is prepared at most once per process.
static TEMPLATES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::OnceCell<()>>>>> =
    OnceLock::new();

mod rows {
    // The `QueryableByName` expansion trips this lint on some toolchains.
    #![allow(clippy::redundant_field_names)]

    use diesel::{
        QueryableByName,
        sql_types::{Nullable, Text},
    };

    #[derive(QueryableByName)]
    pub(super) struct DatabaseRow {
        #[diesel(sql_type = Text)]
        pub(super) datname: String,
        #[diesel(sql_type = Nullable<Text>)]
        pub(super) comment: Option<String>,
    }
}
use rows::DatabaseRow;

/// Runs with the advisory lock held: create + migrate the template if needed, drop stale ones.
fn prepare_template(
    maintenance_url: &str,
    template: &str,
    template_url: &str,
    template_prefix: &str,
    lock_key: i64,
    migrations: EmbeddedMigrations,
) -> Result<(), Error> {
    // The lock is session scoped, so this maintenance connection stays open until the template
    // is ready. It never connects to the template itself.
    let mut conn = PgConnection::establish(maintenance_url).map_err(sww)?;
    diesel::sql_query("SELECT pg_advisory_lock($1)")
        .bind::<BigInt, _>(lock_key)
        .execute(&mut conn)
        .map_err(sww)?;

    let result = (|| {
        let databases: Vec<DatabaseRow> = diesel::sql_query(
            "SELECT datname::text AS datname, shobj_description(oid, 'pg_database') AS comment \
             FROM pg_database",
        )
        .load(&mut conn)
        .map_err(sww)?;

        let ready = databases
            .iter()
            .any(|d| d.datname == template && d.comment.as_deref() == Some(TEMPLATE_READY_MARKER));
        if !ready {
            // Missing, or left half-migrated by an interrupted run.
            let quoted = quote_ident(template);
            conn.batch_execute(&format!("DROP DATABASE IF EXISTS {quoted} WITH (FORCE)"))
                .map_err(sww)?;
            conn.batch_execute(&format!("CREATE DATABASE {quoted}"))
                .map_err(sww)?;
            // Opens and closes its own connection to the template.
            if let Err(e) = crate::diesel::run_migrations_blocking(template_url, migrations) {
                let _ =
                    conn.batch_execute(&format!("DROP DATABASE IF EXISTS {quoted} WITH (FORCE)"));
                return Err(e);
            }
            conn.batch_execute(&format!(
                "COMMENT ON DATABASE {quoted} IS '{TEMPLATE_READY_MARKER}'"
            ))
            .map_err(sww)?;
        }

        for stale in databases
            .iter()
            .filter(|d| d.datname.starts_with(template_prefix) && d.datname != template)
        {
            // Plain DROP fails while the database is in use, which is what we want.
            if let Err(e) =
                conn.batch_execute(&format!("DROP DATABASE {}", quote_ident(&stale.datname)))
            {
                tracing::debug!(database = %stale.datname, "Not dropping stale template: {e}");
            }
        }
        Ok(())
    })();

    let _ = diesel::sql_query("SELECT pg_advisory_unlock($1)")
        .bind::<BigInt, _>(lock_key)
        .execute(&mut conn);
    result
}

fn clone_template(maintenance_url: &str, name: &str, template: &str) -> Result<(), Error> {
    let statement = format!(
        "CREATE DATABASE {} TEMPLATE {}",
        quote_ident(name),
        quote_ident(template)
    );
    let mut attempt = 0;
    loop {
        // A short-lived connection per statement.
        let result = PgConnection::establish(maintenance_url)
            .map_err(|e| e.to_string())
            .and_then(|mut conn| conn.batch_execute(&statement).map_err(|e| e.to_string()));
        match result {
            Ok(()) => return Ok(()),
            // Postgres refuses to clone a template that has an open connection; that only lasts
            // as long as another client's statement, so retry briefly.
            Err(e) if e.contains("is being accessed by other users") && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(something_went_wrong!("{statement}: {e}")),
        }
    }
}

/// Stable 64-bit FNV-1a over the migration names in order.
fn migrations_hash(migrations: &EmbeddedMigrations) -> Result<u64, Error> {
    let list = MigrationSource::<Pg>::migrations(migrations).map_err(sww)?;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for migration in &list {
        let name = migration.name().to_string();
        for byte in name.as_bytes().iter().chain(std::iter::once(&0u8)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(hash)
}

fn with_database(base: &Url, database: &str) -> String {
    let mut url = base.clone();
    url.set_path(&format!("/{database}"));
    url.to_string()
}

fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn sww(e: impl std::fmt::Debug) -> Error {
    something_went_wrong!("{e:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_identifiers() {
        assert_eq!(quote_ident("app_test"), "\"app_test\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn replaces_database_in_url() {
        let base = Url::parse("postgres://u:p@localhost:5432/app_test?sslmode=disable").unwrap();
        assert_eq!(
            with_database(&base, "postgres"),
            "postgres://u:p@localhost:5432/postgres?sslmode=disable"
        );
    }
}
