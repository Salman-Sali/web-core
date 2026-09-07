use std::collections::{BTreeMap, HashMap};

use serde::ser::SerializeStruct;
use web_core_validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct FieldValidationErrors {
    pub fields: FieldErrors,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq, Eq)]
pub struct FieldErrors(pub HashMap<String, FieldErrorKind>);

impl serde::Serialize for FieldErrors {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl serde::Serialize for FieldValidationErrors {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("FieldValidationErrors", 2)?;
        state.serialize_field("error", "Validation error")?;
        state.serialize_field("fields", &self.fields.0)?;
        state.end()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FieldErrorKind {
    Message(String),
    Nested(FieldErrors),
    Array(Vec<Option<FieldErrorKind>>),
}

impl FieldErrorKind {
    pub fn get_message(&self) -> Option<String> {
        match self {
            FieldErrorKind::Message(x) => Some(x.clone()),
            _ => None,
        }
    }
}

impl From<BTreeMap<usize, Box<ValidationErrors>>> for FieldErrorKind {
    fn from(map: BTreeMap<usize, Box<ValidationErrors>>) -> Self {
        let mut map = map;
        let mut errors = Vec::<Option<FieldErrorKind>>::new();
        let Some(max_key) = map.keys().max() else {
            return FieldErrorKind::Array(errors);
        };
        for key in 0..=*max_key {
            match map.remove(&key) {
                Some(x) => {
                    errors.push(Some(FieldErrorKind::Nested((*x).into())));
                }
                None => {
                    errors.push(None);
                }
            }
        }

        return FieldErrorKind::Array(errors);
    }
}

impl From<ValidationError> for FieldErrorKind {
    fn from(error: ValidationError) -> Self {
        if let Some(list_error) = error.list_error {
            let mut list = Vec::<Option<FieldErrorKind>>::new();
            for item in list_error {
                match item {
                    Some(x) => {
                        list.push(Some(FieldErrorKind::Message(x)));
                    }
                    None => list.push(None),
                }
            }
            return FieldErrorKind::Array(list);
        }
        FieldErrorKind::Message(error.message.unwrap_or_default().to_string())
    }
}

impl From<ValidationErrors> for FieldErrors {
    fn from(errors: ValidationErrors) -> Self {
        let mut result = HashMap::new();
        for error in errors.0 {
            let field = error.0.to_string();
            match error.1 {
                ValidationErrorsKind::Struct(validation_errors) => {
                    result.insert(
                        field.clone(),
                        FieldErrorKind::Nested((*validation_errors).into()),
                    );
                }
                ValidationErrorsKind::List(btree_map) => {
                    result.insert(field.clone(), btree_map.into());
                }
                ValidationErrorsKind::Field(validation_errors) => {
                    for x in validation_errors {
                        result.insert(field.clone(), FieldErrorKind::from(x));
                    }
                }
            }
        }
        return FieldErrors(result);
    }
}
