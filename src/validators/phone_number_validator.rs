use validator_async::ValidationError;

pub const INVALID_PHONE_NUMBER_MESSAGE: &str = "Invalid phone number.";
pub async fn validate_phone(phone: &str) -> Result<(), ValidationError> {
    match phonenumber::parse(None, phone) {
        Ok(number) if number.is_valid() => Ok(()),
        _ => {
            Err(ValidationError::new("invalid_phone")
                .with_message(INVALID_PHONE_NUMBER_MESSAGE.into()))
        }
    }
}
