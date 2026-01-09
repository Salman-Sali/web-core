#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub subject: String,
}

impl AuthenticatedUser {
    pub fn new(subject: String) -> Self {
        Self { subject }
    }
}