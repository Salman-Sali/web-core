//! Postgres-backed tests. They need `TEST_DATABASE_URL` (any database URL on a server where the
//! user may create databases; only the server and credentials are used) and are skipped when it
//! is not set.

// The `QueryableByName` expansion below trips this lint on some toolchains.
#![allow(clippy::redundant_field_names)]

use std::time::{Duration, Instant};

use diesel::{
    Connection, ExpressionMethods, PgConnection, QueryDsl, QueryableByName,
    sql_types::{BigInt, Text},
};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use url::Url;
use web_core::{
    diesel::{
        AsyncPaginatedLoad, AsyncPgPoolOptions, PgPoolSize, create_async_pg_pool,
        create_async_pg_pool_with_options, create_async_pg_pool_without_migrations, create_pg_pool,
        create_pg_pool_sized, retain_idle, run_pending_migrations,
    },
    error::Error,
    pagination::{PaginatedResult, PaginationQueryParams},
    test::TestDatabase,
};

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("tests/migrations");

diesel::table! {
    widgets (id) {
        id -> Int4,
        name -> Text,
    }
}

/// `TEST_DATABASE_URL` with its database replaced by `database`, or `None` (test skipped).
fn server_url(database: &str) -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(value) if !value.is_empty() => {
            let mut url = Url::parse(&value).expect("TEST_DATABASE_URL is not a valid URL");
            url.set_path(&format!("/{database}"));
            Some(url.to_string())
        }
        _ => {
            eprintln!("skipping Postgres test: TEST_DATABASE_URL is not set");
            None
        }
    }
}

#[derive(QueryableByName)]
struct Name {
    #[diesel(sql_type = Text)]
    datname: String,
}

/// Names (among `names`) of databases that currently exist on the server.
async fn existing_databases(names: Vec<String>) -> Vec<String> {
    let maintenance = server_url("postgres").unwrap();
    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&maintenance).unwrap();
        let rows: Vec<Name> = diesel::RunQueryDsl::load(
            diesel::sql_query("SELECT datname::text AS datname FROM pg_database"),
            &mut conn,
        )
        .unwrap();
        rows.into_iter()
            .map(|r| r.datname)
            .filter(|n| names.contains(n))
            .collect()
    })
    .await
    .unwrap()
}

async fn drop_database(name: String) {
    let maintenance = server_url("postgres").unwrap();
    tokio::task::spawn_blocking(move || {
        use diesel::connection::SimpleConnection;
        let mut conn = PgConnection::establish(&maintenance).unwrap();
        conn.batch_execute(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
            .unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn create_async_pg_pool_creates_database_and_migrates() {
    let name = format!(
        "web_core_pool_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let Some(url) = server_url(&name) else {
        return;
    };
    assert!(existing_databases(vec![name.clone()]).await.is_empty());

    let pool = create_async_pg_pool(MIGRATIONS, &url, 2).await.unwrap();
    assert_eq!(pool.status().max_size, 2);
    assert_eq!(
        existing_databases(vec![name.clone()]).await,
        vec![name.clone()]
    );

    {
        let mut conn = pool.get().await.map_err(Error::from).unwrap();
        for n in ["a", "b", "c", "d", "e"] {
            diesel::insert_into(widgets::table)
                .values(widgets::name.eq(n))
                .execute(&mut conn)
                .await
                .unwrap();
        }

        // Async pagination against the real database.
        let params = PaginationQueryParams {
            additional_params: (),
            page: Some(2),
            per_page: Some(2),
        };
        let page: PaginatedResult<String> = widgets::table
            .select(widgets::name)
            .order(widgets::name)
            .paginated_load::<String, String>(&params, &mut conn)
            .await
            .unwrap();
        assert_eq!(page.data, vec!["c".to_string(), "d".to_string()]);
        assert_eq!(page.total, 5);
        assert_eq!(page.total_pages, 3);
        assert_eq!(page.page, 2);

        // Real unique violation -> 409 naming the constraint.
        let err = diesel::insert_into(widgets::table)
            .values(widgets::name.eq("a"))
            .execute(&mut conn)
            .await
            .unwrap_err();
        match Error::from(err) {
            Error::Conflict(c) => assert_eq!(c.error, "Conflict on widgets_name_key"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    // Running again on the existing database is a no-op for migrations.
    drop(create_async_pg_pool(MIGRATIONS, &url, 1).await.unwrap());

    drop(pool);
    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_database_creates_isolated_databases_and_drops_them() {
    // A dedicated prefix so this never touches other projects' templates on the same server.
    let Some(base) = server_url("web_core_test") else {
        return;
    };

    let (a, b) = tokio::join!(
        TestDatabase::create(&base, MIGRATIONS),
        TestDatabase::create(&base, MIGRATIONS)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.name, b.name);
    assert_ne!(a.url, b.url);
    for db in [&a, &b] {
        assert!(db.name.starts_with("web_core_test_"));
        assert!(!db.name.contains("_tpl_"));
    }
    let names = vec![a.name.clone(), b.name.clone()];
    assert_eq!(existing_databases(names.clone()).await.len(), 2);

    // Both have the migrated table and are isolated from each other.
    {
        let mut conn_a = AsyncPgConnection::establish(&a.url).await.unwrap();
        let mut conn_b = AsyncPgConnection::establish(&b.url).await.unwrap();
        diesel::insert_into(widgets::table)
            .values(widgets::name.eq("only-in-a"))
            .execute(&mut conn_a)
            .await
            .unwrap();
        let count_a: i64 = widgets::table
            .count()
            .get_result(&mut conn_a)
            .await
            .unwrap();
        let count_b: i64 = widgets::table
            .count()
            .get_result(&mut conn_b)
            .await
            .unwrap();
        assert_eq!((count_a, count_b), (1, 0));
    }

    // A third one reuses the already prepared template.
    let c = TestDatabase::create(&base, MIGRATIONS).await.unwrap();
    let mut all = names.clone();
    all.push(c.name.clone());

    drop(a);
    drop(b);
    drop(c);
    assert!(existing_databases(all).await.is_empty());
}

fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )
}

async fn create_database(name: String) {
    let maintenance = server_url("postgres").unwrap();
    tokio::task::spawn_blocking(move || {
        use diesel::connection::SimpleConnection;
        let mut conn = PgConnection::establish(&maintenance).unwrap();
        conn.batch_execute(&format!("CREATE DATABASE \"{name}\""))
            .unwrap();
    })
    .await
    .unwrap();
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

/// Number of server backends connected to database `name`.
async fn connection_count(name: &str) -> i64 {
    let maintenance = server_url("postgres").unwrap();
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut conn = PgConnection::establish(&maintenance).unwrap();
        let rows: Vec<Count> = diesel::RunQueryDsl::load(
            diesel::sql_query("SELECT count(*) AS count FROM pg_stat_activity WHERE datname = $1")
                .bind::<Text, _>(name),
            &mut conn,
        )
        .unwrap();
        rows[0].count
    })
    .await
    .unwrap()
}

/// Waits (up to 10 s) until database `name` has `expected` connections; backends exit
/// asynchronously after the client closes the socket.
async fn wait_for_connections(name: &str, expected: i64) {
    let start = Instant::now();
    loop {
        let count = connection_count(name).await;
        if count == expected {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "database {name} has {count} connections, expected {expected}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn widget_count(pool: &web_core::diesel::AsyncPgPool) -> i64 {
    let mut conn = pool.get().await.unwrap();
    widgets::table.count().get_result(&mut conn).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_without_migrations_is_lazy_and_never_creates_the_database() {
    let name = unique_name("web_core_tenant");
    let Some(url) = server_url(&name) else {
        return;
    };

    // Missing database: building succeeds, the first get() fails, nothing is created.
    let pool = create_async_pg_pool_without_migrations(&url, 2).unwrap();
    assert_eq!(pool.status().max_size, 2);
    assert!(pool.get().await.is_err());
    assert!(existing_databases(vec![name.clone()]).await.is_empty());
    drop(pool);

    // Existing database: no connection until first use, no migrations run.
    create_database(name.clone()).await;
    let pool = create_async_pg_pool_with_options(
        &url,
        AsyncPgPoolOptions::new(2)
            .with_wait_timeout(Duration::from_secs(5))
            .with_create_timeout(Duration::from_secs(5))
            .with_recycle_timeout(Duration::from_secs(5)),
    )
    .unwrap();
    assert_eq!(pool.status().size, 0);
    assert_eq!(connection_count(&name).await, 0);
    {
        let mut conn = pool.get().await.unwrap();
        let missing = widgets::table.count().get_result::<i64>(&mut conn).await;
        assert!(missing.is_err(), "migrations must not have run");
    }
    assert_eq!(connection_count(&name).await, 1);

    // Explicit migration step, then the table exists.
    run_pending_migrations(MIGRATIONS, &url).await.unwrap();
    run_pending_migrations(MIGRATIONS, &url).await.unwrap();
    assert_eq!(widget_count(&pool).await, 0);

    drop(pool);
    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retain_idle_closes_only_idle_connections() {
    let name = unique_name("web_core_reaper");
    let Some(url) = server_url(&name) else {
        return;
    };
    create_database(name.clone()).await;
    let pool = create_async_pg_pool_without_migrations(&url, 3).unwrap();

    // Open three connections and return them to the pool.
    {
        let (a, b, c) = tokio::join!(pool.get(), pool.get(), pool.get());
        let _conns = (a.unwrap(), b.unwrap(), c.unwrap());
    }
    assert_eq!(pool.status().size, 3);
    wait_for_connections(&name, 3).await;

    // Nothing is idle for long enough yet.
    assert_eq!(retain_idle(&pool, Duration::from_secs(60)), 0);
    assert_eq!(pool.status().size, 3);

    tokio::time::sleep(Duration::from_millis(1200)).await;

    // One connection is checked out (fresh use); the two idle ones are closed.
    let held = pool.get().await.unwrap();
    assert_eq!(retain_idle(&pool, Duration::from_secs(1)), 2);
    assert_eq!(pool.status().size, 1);
    wait_for_connections(&name, 1).await;

    // Returned, but used just now: kept.
    drop(held);
    assert_eq!(retain_idle(&pool, Duration::from_secs(1)), 0);
    assert_eq!(pool.status().size, 1);

    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(retain_idle(&pool, Duration::from_secs(1)), 1);
    assert_eq!(pool.status().size, 0);
    wait_for_connections(&name, 0).await;

    // The pool still works after being emptied.
    let conn = pool.get().await.unwrap();
    drop(conn);
    wait_for_connections(&name, 1).await;

    drop(pool);
    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_pool_closes_its_connections() {
    let name = unique_name("web_core_droppool");
    let Some(url) = server_url(&name) else {
        return;
    };
    create_database(name.clone()).await;

    // Idle connections close when the last pool handle is dropped.
    let pool = create_async_pg_pool_without_migrations(&url, 2).unwrap();
    let clone = pool.clone();
    {
        let (a, b) = tokio::join!(pool.get(), pool.get());
        let _conns = (a.unwrap(), b.unwrap());
    }
    wait_for_connections(&name, 2).await;
    drop(pool);
    // A clone keeps the pool (and its connections) alive.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(connection_count(&name).await, 2);
    drop(clone);
    wait_for_connections(&name, 0).await;

    // A connection checked out when the pool is dropped closes when it is released.
    let pool = create_async_pg_pool_without_migrations(&url, 2).unwrap();
    let held = {
        let (a, b) = tokio::join!(pool.get(), pool.get());
        let held = a.unwrap();
        drop(b);
        held
    };
    wait_for_connections(&name, 2).await;
    drop(pool);
    wait_for_connections(&name, 1).await;
    drop(held);
    wait_for_connections(&name, 0).await;

    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_pg_pool_keeps_r2d2_defaults() {
    let name = unique_name("web_core_syncpool");
    let Some(url) = server_url(&name) else {
        return;
    };

    // Creates the database, migrates and opens all 10 connections up front.
    let pool = create_pg_pool(MIGRATIONS, &url).await.unwrap();
    assert_eq!(pool.max_size(), 10);
    assert_eq!(pool.min_idle(), None);
    assert_eq!(pool.state().connections, 10);
    wait_for_connections(&name, 10).await;
    {
        let mut conn = pool.get().unwrap();
        diesel::RunQueryDsl::execute(
            diesel::insert_into(widgets::table).values(widgets::name.eq("a")),
            &mut conn,
        )
        .unwrap();
    }

    drop(pool);
    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_pg_pool_sized_honours_max_size_and_min_idle() {
    let name = unique_name("web_core_sizedpool");
    let Some(url) = server_url(&name) else {
        return;
    };

    let pool = create_pg_pool_sized(MIGRATIONS, &url, PgPoolSize::new(3).with_min_idle(1))
        .await
        .unwrap();
    assert_eq!(
        existing_databases(vec![name.clone()]).await,
        vec![name.clone()]
    );
    assert_eq!(pool.max_size(), 3);
    assert_eq!(pool.min_idle(), Some(1));
    // Only `min_idle` connections are opened at startup.
    assert_eq!(pool.state().connections, 1);
    wait_for_connections(&name, 1).await;

    {
        let conns: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
        assert_eq!(pool.state().connections, 3);
        // No fourth connection.
        assert!(pool.get_timeout(Duration::from_millis(200)).is_err());
        let mut conn = conns.into_iter().next().unwrap();
        diesel::RunQueryDsl::execute(
            diesel::insert_into(widgets::table).values(widgets::name.eq("a")),
            &mut conn,
        )
        .unwrap();
    }
    drop(pool);

    // `min_idle(0)` opens nothing until the first `get()`; migrations are a no-op the second time.
    let pool = create_pg_pool_sized(MIGRATIONS, &url, PgPoolSize::new(2).with_min_idle(0))
        .await
        .unwrap();
    assert_eq!(pool.state().connections, 0);
    {
        let mut conn = pool.get().unwrap();
        let count: i64 =
            diesel::RunQueryDsl::get_result(widgets::table.count(), &mut conn).unwrap();
        assert_eq!(count, 1);
    }
    assert_eq!(pool.state().connections, 1);

    drop(pool);
    drop_database(name.clone()).await;
    assert!(existing_databases(vec![name]).await.is_empty());
}

#[tokio::test]
async fn create_pg_pool_sized_rejects_invalid_sizes_without_connecting() {
    // Unreachable URL: the size check fails before any connection attempt.
    let url = "postgres://nobody@127.0.0.1:1/web_core_never_created";
    assert_eq!(PgPoolSize::default(), PgPoolSize::new(10));
    let err = create_pg_pool_sized(MIGRATIONS, url, PgPoolSize::new(0))
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("max_size must be positive"),
        "{err:?}"
    );
    let err = create_pg_pool_sized(MIGRATIONS, url, PgPoolSize::new(2).with_min_idle(3))
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("min_idle (3)"), "{err:?}");
}
