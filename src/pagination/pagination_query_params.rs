use crate::pagination::Pagination;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaginationQueryParams<T> {
    #[serde(flatten)]
    pub additional_params: T,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

impl<T> Pagination for PaginationQueryParams<T> {
    fn page(&self) -> i64 {
        self.page.unwrap_or(1).max(1)
    }

    fn per_page(&self) -> i64 {
        self.per_page.unwrap_or(10).clamp(1, 100)
    }

    fn offset(&self) -> i64 {
        (self.page() - 1) * self.per_page()
    }
}
