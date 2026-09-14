use diesel::pg::Pg;
use diesel::query_builder::{AstPass, Query, QueryFragment, QueryId};
use diesel::query_dsl::methods::LoadQuery;
use diesel::sql_types::BigInt;
use diesel::{PgConnection, QueryResult, RunQueryDsl};

use crate::pagination::{PaginatedResult, Pagination};

#[derive(Clone, Copy, QueryId)]
pub struct Paginated<T> {
    query: T,
    per_page: i64,
    offset: i64,
}

impl<T: Query> Query for Paginated<T> {
    type SqlType = (T::SqlType, BigInt);
}

impl<T> RunQueryDsl<PgConnection> for Paginated<T> {}

impl<T> QueryFragment<Pg> for Paginated<T>
where
    T: QueryFragment<Pg>,
{
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> QueryResult<()> {
        out.push_sql("SELECT *, COUNT(*) OVER () FROM (");
        self.query.walk_ast(out.reborrow())?;
        out.push_sql(") t LIMIT ");
        out.push_bind_param::<BigInt, _>(&self.per_page)?;
        out.push_sql(" OFFSET ");
        out.push_bind_param::<BigInt, _>(&self.offset)?;
        Ok(())
    }
}

pub trait PaginatedLoad: Sized + Query {
    fn paginated_load<'query, T, R>(
        self,
        pagination: &impl Pagination,
        conn: &mut PgConnection,
    ) -> QueryResult<PaginatedResult<R>>
    where
        Paginated<Self>: LoadQuery<'query, PgConnection, (T, i64)>,
        R: From<T>,
    {
        let wrapped = Paginated {
            query: self,
            per_page: pagination.per_page(),
            offset: pagination.offset(),
        };
        let results = wrapped.load::<(T, i64)>(conn)?;
        let total = results.first().map(|(_, t)| *t).unwrap_or(0);
        let items = results.into_iter().map(|(item, _)| item).collect();
        Ok(PaginatedResult::new(items, total, pagination))
    }
}

impl<Q: Query> PaginatedLoad for Q {}
