use std::collections::HashMap;

#[derive(Debug, serde::Deserialize)]
pub struct FieldValidationErrors {
    pub fields: HashMap<String, String>,
}

impl serde::Serialize for FieldValidationErrors {
    fn serialize<__S>(&self, __serializer: __S) -> serde::__private228::Result<__S::Ok, __S::Error>
    where
        __S: serde::Serializer,
    {
        let mut __serde_state = serde::Serializer::serialize_struct(__serializer, "FieldValidationErrors", false as usize + 1)?;
        serde::ser::SerializeStruct::serialize_field(&mut __serde_state, "error", "Validation failed")?;
        serde::ser::SerializeStruct::serialize_field(&mut __serde_state, "fields", &self.fields)?;
        serde::ser::SerializeStruct::end(__serde_state)
    }
}

