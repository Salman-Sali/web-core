use std::collections::HashMap;

use serde_json::Value;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BadRequestError {
    pub error: String,
    #[serde(default)]
    pub data: HashMap<String, String>,
    pub _data: Option<Value>
}

impl BadRequestError {
    pub fn new(error: String) -> Self {
        Self { error, data: HashMap::new(), _data: None }
    }

    pub fn new_with_data(error: String, data: Value) -> Self {
        Self { error, data: HashMap::new(), _data: Some(data) }
    }
}

impl serde::Serialize for BadRequestError {
    fn serialize<__S>(&self, __serializer: __S) -> serde::__private228::Result<__S::Ok, __S::Error>
    where
        __S: serde::Serializer,
    {
        let mut _serde_state = serde::Serializer::serialize_struct(__serializer, "BadRequestError", false as usize + 1)?;
        serde::ser::SerializeStruct::serialize_field(&mut _serde_state, "error", &self.error)?;
        if self.data.len() != 0 {
            serde::ser::SerializeStruct::serialize_field(&mut _serde_state, "data", &self.data)?;
        }
        if let Some(data) = &self._data {
            serde::ser::SerializeStruct::serialize_field(&mut _serde_state, "data", data)?;
        }
        serde::ser::SerializeStruct::end(_serde_state)
    }
}
