use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A document identifier, unique within a single index.
pub type DocumentId = String;

/// An engine-agnostic document: an optional id plus a bag of named fields.
///
/// This mirrors the role of a row in SQLAlchemy Core - a plain, dynamically
/// typed record that any backend can serialize into its own storage format.
/// Application code typically converts its own structs to/from `Document`
/// via `serde_json`, e.g. `Document::from_serializable(&my_struct)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub id: Option<DocumentId>,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl Document {
    /// Creates an empty document with no id and no fields.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the document id, consuming and returning `self` for chaining.
    pub fn with_id(mut self, id: impl Into<DocumentId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Sets a field value, consuming and returning `self` for chaining.
    pub fn set(mut self, field: impl Into<String>, value: impl Into<Value>) -> Self {
        self.fields.insert(field.into(), value.into());
        self
    }

    /// Reads a field's raw JSON value, if present.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.get(field)
    }

    /// Builds a `Document` from any `Serialize` type. The type must
    /// serialize to a JSON object; an `id` field, if present, is pulled out
    /// into [`Document::id`].
    pub fn from_serializable<T: Serialize>(value: &T) -> Result<Self, serde_json::Error> {
        let json = serde_json::to_value(value)?;
        let mut fields = match json {
            Value::Object(map) => map,
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected a JSON object, got {other}"
                )))
            }
        };
        let id = fields.remove("id").and_then(|v| match v {
            Value::String(s) => Some(s),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
        Ok(Document { id, fields })
    }

    /// Deserializes the document's fields (and `id`, if the target type has
    /// one) into a concrete type.
    pub fn into_serializable<T: for<'de> Deserialize<'de>>(self) -> Result<T, serde_json::Error> {
        let mut fields = self.fields;
        if let Some(id) = self.id {
            fields.insert("id".to_string(), Value::String(id));
        }
        serde_json::from_value(Value::Object(fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Article {
        id: String,
        title: String,
        views: u32,
    }

    #[test]
    fn builder_sets_id_and_fields() {
        let doc = Document::new().with_id("1").set("title", "hello");
        assert_eq!(doc.id.as_deref(), Some("1"));
        assert_eq!(doc.get("title"), Some(&Value::String("hello".into())));
    }

    #[test]
    fn roundtrips_through_serializable() {
        let article = Article {
            id: "42".to_string(),
            title: "Rust".to_string(),
            views: 7,
        };
        let doc = Document::from_serializable(&article).unwrap();
        assert_eq!(doc.id.as_deref(), Some("42"));
        assert_eq!(doc.get("title"), Some(&Value::String("Rust".into())));

        let back: Article = doc.into_serializable().unwrap();
        assert_eq!(back, article);
    }

    #[test]
    fn from_serializable_rejects_non_object() {
        let err = Document::from_serializable(&42).unwrap_err();
        assert!(err.to_string().contains("expected a JSON object"));
    }
}
