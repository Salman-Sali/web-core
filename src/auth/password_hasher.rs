use uuid::Uuid;
use argon2::{password_hash::Salt, PasswordHash, PasswordHasher, PasswordVerifier};

use crate::{error::Error, something_went_wrong};

pub fn hash_password(password: &str) -> Result<String, Error> {
    let argon2 = argon2::Argon2::default();
    let salt_str = Uuid::new_v4().to_string().replace("-", "");
    let salt: Salt = salt_str.as_str().try_into()
        .map_err(|e| something_went_wrong!("Error while creating salt for password hash : {e}"))?;

    return argon2.hash_password(password.as_bytes(), salt)
        .map(|x| x.to_string())
        .map_err(|e| something_went_wrong!("Error while hashing password : {e}"));
}

pub fn verify_password(password: String, hash: String) -> Result<bool, Error> {
    let argon2 = argon2::Argon2::default();
    let hash = PasswordHash::new(&hash)
        .map_err(|e| something_went_wrong!("Error while parsing hash : {e}"))?;
    let verify_password_result = argon2.verify_password(password.as_bytes(), &hash).map_or_else(|_| false, |_| true);
    return Ok(verify_password_result);
}

pub trait PasswordHandler {
    fn get_password_hash(&self) -> String;
    fn set_password_hash(&mut self, new_hashed_password: String);
}

pub trait PasswordHandlerExtensions {
    fn update_password(&mut self, password: &str) -> Result<(), Error>;
    fn validate_password(&self, password: String) -> Result<bool, Error>;
}

impl<T> PasswordHandlerExtensions for T where T: PasswordHandler {
    fn update_password(&mut self, password: &str) -> Result<(), Error> {
        let hashed_password = hash_password(password)?;
        self.set_password_hash(hashed_password);
        return Ok(());
    }
    
    fn validate_password(&self, password: String) -> Result<bool, Error> {
        let hash = self.get_password_hash();
        verify_password(password, hash)
    }    
}