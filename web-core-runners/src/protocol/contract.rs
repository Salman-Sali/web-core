use schemars::{JsonSchema, Schema, schema_for};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::Task;

/// Serialises `value` canonically: object keys sorted, no whitespace. Two equal JSON values
/// always give the same string, whatever order their maps were built in.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                if let Some(v) = map.get(*key) {
                    write_canonical(v, out);
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Removes string-valued `description` keys (doc comments), so editing a comment on a contract
/// type does not force every runner to update.
fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if matches!(map.get("description"), Some(Value::String(_))) {
                map.remove("description");
            }
            map.values_mut().for_each(strip_descriptions);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

/// SHA-256 (lowercase hex) of the canonical JSON of `schemas`, in the given order, with doc
/// comment descriptions removed. The building block of [`Contract::hash`].
pub fn contract_hash(schemas: &[Schema]) -> String {
    let values: Vec<Value> = schemas
        .iter()
        .map(|s| {
            let mut v = s.as_value().clone();
            strip_descriptions(&mut v);
            v
        })
        .collect();
    hex::encode(Sha256::digest(canonical_json(&Value::Array(values)).as_bytes()))
}

/// The set of task types of a product, fingerprinted for the `Hello` handshake.
///
/// A runner whose contract hash differs from the app's gets no work. The hash covers each
/// task's `TYPE` and the JSON schemas of its input and output; tasks are sorted by type, so
/// registration order does not matter.
///
/// ```
/// # use schemars::JsonSchema;
/// # use serde::{Deserialize, Serialize};
/// # use web_core_runners::protocol::{Contract, Task};
/// # pub struct Thumbnail;
/// # #[derive(Serialize, Deserialize, JsonSchema)] pub struct In { pub w: u32 }
/// # #[derive(Serialize, Deserialize, JsonSchema)] pub struct Out { pub h: u32 }
/// # impl Task for Thumbnail { const TYPE: &'static str = "thumbnail"; type Input = In; type Output = Out; }
/// use std::sync::OnceLock;
///
/// /// The product contract crate's fingerprint, computed once.
/// pub fn contract_hash() -> &'static str {
///     static HASH: OnceLock<String> = OnceLock::new();
///     HASH.get_or_init(|| Contract::new().task::<Thumbnail>().hash())
/// }
/// ```
#[derive(Debug, Default, Clone)]
pub struct Contract {
    tasks: Vec<(String, Schema, Schema)>,
}

impl Contract {
    /// An empty contract.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds task type `T`.
    pub fn task<T: Task>(mut self) -> Self {
        self.tasks.push((
            T::TYPE.to_string(),
            schema_for!(T::Input),
            schema_for!(T::Output),
        ));
        self
    }

    /// Adds a raw schema pair under `name` (for tasks without a [`Task`] impl).
    pub fn raw<I: JsonSchema, O: JsonSchema>(mut self, name: &str) -> Self {
        self.tasks
            .push((name.to_string(), schema_for!(I), schema_for!(O)));
        self
    }

    /// Task type names in this contract, sorted.
    pub fn task_types(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tasks.iter().map(|t| t.0.clone()).collect();
        names.sort();
        names
    }

    /// The fingerprint: SHA-256 hex of the canonical schema set.
    pub fn hash(&self) -> String {
        let mut tasks: Vec<&(String, Schema, Schema)> = self.tasks.iter().collect();
        tasks.sort_by(|a, b| a.0.cmp(&b.0));
        let mut schemas = Vec::with_capacity(tasks.len() * 3);
        for (name, input, output) in tasks {
            schemas.push(Schema::from(serde_json::Map::from_iter([(
                "task".to_string(),
                Value::String(name.clone()),
            )])));
            schemas.push(input.clone());
            schemas.push(output.clone());
        }
        contract_hash(&schemas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_and_drops_whitespace() {
        let a: Value = serde_json::from_str(r#"{ "b": 1, "a": { "y": [1, 2], "x": "s" } }"#)
            .expect("json");
        assert_eq!(canonical_json(&a), r#"{"a":{"x":"s","y":[1,2]},"b":1}"#);
        let b = json!({"a": {"y": [1, 2], "x": "s"}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
    }

    #[test]
    fn canonical_json_escapes_keys() {
        let v = json!({"a\"b": "c\nd"});
        assert_eq!(canonical_json(&v), r#"{"a\"b":"c\nd"}"#);
    }

    struct Alpha;
    struct Beta;

    /// Input of alpha.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct AlphaIn {
        /// Width in pixels.
        width: u32,
    }
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct AlphaOut {
        ok: bool,
    }

    impl Task for Alpha {
        const TYPE: &'static str = "alpha";
        type Input = AlphaIn;
        type Output = AlphaOut;
    }

    impl Task for Beta {
        const TYPE: &'static str = "beta";
        type Input = AlphaOut;
        type Output = AlphaIn;
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct AlphaInV2 {
        width: u32,
        height: u32,
    }

    #[test]
    fn contract_hash_is_stable_and_order_independent() {
        let a = Contract::new().task::<Alpha>().task::<Beta>().hash();
        let b = Contract::new().task::<Beta>().task::<Alpha>().hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(a, Contract::new().task::<Alpha>().task::<Beta>().hash());
    }

    #[test]
    fn contract_hash_changes_with_shape_or_name() {
        let base = Contract::new().raw::<AlphaIn, AlphaOut>("alpha").hash();
        assert_eq!(base, Contract::new().task::<Alpha>().hash());
        assert_ne!(base, Contract::new().raw::<AlphaInV2, AlphaOut>("alpha").hash());
        assert_ne!(base, Contract::new().raw::<AlphaIn, AlphaOut>("alpha2").hash());
    }

    #[test]
    fn contract_hash_ignores_descriptions() {
        let mut with_doc = schema_for!(AlphaIn);
        let plain = with_doc.clone();
        with_doc.insert("description".into(), json!("changed doc comment"));
        assert_eq!(contract_hash(&[plain]), contract_hash(&[with_doc]));
    }

    #[test]
    fn contract_hash_known_value() {
        let schema = Schema::from(serde_json::Map::from_iter([(
            "type".to_string(),
            json!("string"),
        )]));
        // sha256 of `[{"type":"string"}]`
        assert_eq!(
            contract_hash(&[schema]),
            hex::encode(Sha256::digest(br#"[{"type":"string"}]"#))
        );
    }
}
