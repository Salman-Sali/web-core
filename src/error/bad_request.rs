use std::collections::HashMap;

use serde_json::Value;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BadRequestError {
    pub error: String,
    #[serde(default)]
    pub data: HashMap<String, String>,
    pub _data: Option<Value>,
}

impl BadRequestError {
    pub fn new(error: String) -> Self {
        Self {
            error,
            data: HashMap::new(),
            _data: None,
        }
    }

    pub fn new_with_data(error: String, data: Value) -> Self {
        Self {
            error,
            data: HashMap::new(),
            _data: Some(data),
        }
    }
}

use serde::ser::SerializeStruct;
use std::result::Result;

impl serde::Serialize for BadRequestError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let has_data = !self.data.is_empty() || self._data.is_some();
        let field_count = if has_data { 2 } else { 1 };

        let mut state = serializer.serialize_struct("BadRequestError", field_count)?;
        state.serialize_field("error", &self.error)?;

        if !self.data.is_empty() {
            state.serialize_field("data", &self.data)?;
        }
        if let Some(data) = &self._data {
            state.serialize_field("data", data)?;
        }

        state.end()
    }
}
