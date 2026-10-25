use std::future::Future;

use diesel::QueryResult;
use diesel::query_builder::Query;
use diesel_async::AsyncPgConnection;
use diesel_async::methods::LoadQuery;

use super::paginated_load::Paginated;
use crate::pagination::{PaginatedResult, Pagination};

/// Async (diesel-async / `AsyncPgConnection`) pagination, feature `diesel-async`.
///
/// Wraps the query exactly like the sync [`PaginatedLoad`](super::PaginatedLoad):
/// `SELECT *, COUNT(*) OVER () FROM (<query>) t LIMIT $1 OFFSET $2`, and returns the page
/// together with the total row count.
///
/// A project imports either `PaginatedLoad` or `AsyncPaginatedLoad`, not both: both are
/// blanket-implemented for every `Query` and share the method name `paginated_load`, so having
/// both in scope makes calls ambiguous.
///
/// The returned future is `Send`, so it can be awaited inside axum handlers.
///
/// ```rust,ignore
/// use web_core::diesel::AsyncPaginatedLoad;
///
/// let page: PaginatedResult<ProductDto> = products::table
///     .select(Product::as_select())
///     .order(products::id)
///     .paginated_load::<Product, ProductDto>(&params, &mut conn)
///     .await?;
/// ```
pub trait AsyncPaginatedLoad: Sized + Query {
    fn paginated_load<'query, 'conn, T, R>(
        self,
        pagination: &impl Pagination,
        conn: &'conn mut AsyncPgConnection,
    ) -> impl Future<Output = QueryResult<PaginatedResult<R>>> + Send
    where
        Self: Send + 'query,
        Paginated<Self>: LoadQuery<'query, AsyncPgConnection, (T, i64)> + 'query,
        T: Send + 'static,
        R: From<T>,
    {
        let wrapped = Paginated::new(self, pagination);
        let page = PageSnapshot {
            page: pagination.page(),
            per_page: pagination.per_page(),
            offset: pagination.offset(),
        };
        async move {
            // Called through the trait path: `diesel_async::RunQueryDsl` is implemented for every type,
            // so importing it would shadow `Vec::first` below.
            let results: Vec<(T, i64)> =
                diesel_async::RunQueryDsl::load::<(T, i64)>(wrapped, conn).await?;
            let total = results.first().map(|(_, t)| *t).unwrap_or(0);
            let items: Vec<T> = results.into_iter().map(|(item, _)| item).collect();
            Ok(PaginatedResult::new(items, total, &page))
        }
    }
}

impl<Q: Query> AsyncPaginatedLoad for Q {}

/// Copy of the caller's pagination values so the returned future does not borrow it.
struct PageSnapshot {
    page: i64,
    per_page: i64,
    offset: i64,
}

impl Pagination for PageSnapshot {
    fn page(&self) -> i64 {
        self.page
    }

    fn per_page(&self) -> i64 {
        self.per_page
    }

    fn offset(&self) -> i64 {
        self.offset
    }
}

#[cfg(test)]
mod tests {
    use diesel::pg::Pg;
    use diesel::prelude::*;

    use super::*;
    use crate::pagination::PaginationQueryParams;

    diesel::table! {
        widgets (id) {
            id -> Int4,
            name -> Text,
        }
    }

    #[test]
    fn async_paginated_query_renders_wrapped_sql() {
        let params = PaginationQueryParams {
            additional_params: (),
            page: Some(3),
            per_page: Some(20),
        };
        let query = widgets::table
            .select((widgets::id, widgets::name))
            .filter(widgets::name.eq("a"))
            .order(widgets::id);
        let wrapped = Paginated::new(query, &params);
        let sql = diesel::debug_query::<Pg, _>(&wrapped).to_string();
        assert_eq!(
            sql,
            "SELECT *, COUNT(*) OVER () FROM (SELECT \"widgets\".\"id\", \"widgets\".\"name\" \
             FROM \"widgets\" WHERE (\"widgets\".\"name\" = $1) ORDER BY \"widgets\".\"id\") t \
             LIMIT $2 OFFSET $3 -- binds: [\"a\", 20, 40]"
        );
    }

    /// Compile-time check: the future returned by `paginated_load` is `Send` for a regular query.
    #[allow(dead_code)]
    fn paginated_load_future_is_send(conn: &mut AsyncPgConnection) {
        fn assert_send<F: Send>(_: F) {}
        let params = PaginationQueryParams {
            additional_params: (),
            page: None,
            per_page: None,
        };
        assert_send(
            widgets::table
                .select((widgets::id, widgets::name))
                .paginated_load::<(i32, String), (i32, String)>(&params, conn),
        );
    }
}
