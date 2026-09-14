use crate::pagination::Pagination;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct PaginatedResult<T> {
    pub data: Vec<T>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
    pub total_pages: i64,
}

impl<T> PaginatedResult<T> {
    pub fn new(data: Vec<impl Into<T>>, total: i64, query: &impl Pagination) -> Self {
        let per_page = query.per_page();
        let total_pages = if total == 0 {
            0
        } else {
            (total + per_page - 1) / per_page
        };

        Self {
            data: data.into_iter().map(|x| x.into()).collect(),
            page: query.page(),
            per_page,
            total,
            total_pages,
        }
    }
}
