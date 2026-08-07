use std::collections::HashMap;

use serde::ser::SerializeStruct;

#[derive(Debug, serde::Deserialize)]
pub struct FieldValidationErrors {
    pub fields: HashMap<String, String>,
}

impl serde::Serialize for FieldValidationErrors {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("FieldValidationErrors", 2)?;
        state.serialize_field("error", "Validation failed")?;
        state.serialize_field("fields", &self.fields)?;
        state.end()
    }
}
