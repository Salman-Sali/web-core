mod paginated_result;
mod pagination_query_params;

pub use paginated_result::PaginatedResult;
pub use pagination_query_params::PaginationQueryParams;

pub trait Pagination {
    fn page(&self) -> i64;
    fn per_page(&self) -> i64;
    fn offset(&self) -> i64;
}
