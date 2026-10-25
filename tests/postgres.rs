//! Postgres-backed tests. They need `TEST_DATABASE_URL` (any database URL on a server where the
//! user may create databases; only the server and credentials are used) and are skipped when it
//! is not set.

// The `QueryableByName` expansion below trips this lint on some toolchains.
#![allow(clippy::redundant_field_names)]

use diesel::{
    Connection, ExpressionMethods, PgConnection, QueryDsl, QueryableByName, sql_types::Text,
};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use url::Url;
use web_core::{
    diesel::{AsyncPaginatedLoad, create_async_pg_pool},
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
