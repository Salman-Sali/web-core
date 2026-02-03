use std::str::FromStr;

use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey, ed25519::signature::SignerMut};

use crate::{error::Error, something_went_wrong};

pub fn sign(signing_key: &str, data: &str) -> Result<String, Error> {
    let key_bytes = hex::decode(signing_key).map_err(|e| something_went_wrong!("{:?}", e))?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    let mut loaded_signing_key = SigningKey::from_bytes(&key_array);
    let signature = loaded_signing_key.sign(data.as_bytes());
    Ok(signature.to_string())
}

pub fn verify(verifying_key: &str, data: &str, signature: &str) -> Result<(), Error> {
    let key_bytes = hex::decode(verifying_key).map_err(|e| something_went_wrong!("{:?}", e))?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|e| something_went_wrong!("{:?}", e))?;
    let verifying_key =
        VerifyingKey::from_bytes(&key_array).map_err(|e| something_went_wrong!("{:?}", e))?;
    let signature = Signature::from_str(signature).map_err(|e| something_went_wrong!("{:?}", e))?;
    verifying_key
        .verify(data.as_bytes(), &signature)
        .map_err(|e| something_went_wrong!("{:?}", e))
}
