use google_oauth::AsyncClient;

use crate::{error::Error, something_went_wrong};
pub async  fn verify_google_token(client_id: String, id_token: String) -> Result<String,Error > {
    let client = AsyncClient::new(client_id.clone());

    let payload = client
        .validate_id_token(id_token)
        .await
        .map_err(|e| something_went_wrong!("Error while validating google id token : {e:?}"))?;

    if payload.aud != client_id || payload.email_verified != Some(true) {
        return Err(something_went_wrong!("Invlalid token"))?;
    }

    let email = payload
        .email
        .ok_or_else(|| something_went_wrong!("invalid email"))?;

    Ok(email)
}