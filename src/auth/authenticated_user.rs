use std::any::type_name;

use crate::{error::Error, something_went_wrong};

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub subject: String,
}

impl AuthenticatedUser {
    pub fn new(subject: String) -> Self {
        Self { subject }
    }

    pub fn get_user_id<T: TryFrom<String>>(&self) -> Result<T, Error> {
        self.subject.clone().try_into().map_err(|_| {
            something_went_wrong!(
                "Error while converting String subject to {}",
                type_name::<T>()
            )
        })
    }
}
