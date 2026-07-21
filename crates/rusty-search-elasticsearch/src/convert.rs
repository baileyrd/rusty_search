use rusty_search_core::{Document, FieldType as CoreFieldType, SearchError};
use serde_json::Value as JsonValue;

/// Coerces a `Query::Term`/range-bound string value into the JSON
/// representation Elasticsearch expects for `field_type`, since the core
/// `Query` DSL carries term values as plain strings regardless of the
/// field's real type.
pub fn coerce_term_value(field_type: CoreFieldType, value: &str) -> Result<JsonValue, SearchError> {
    match field_type {
        CoreFieldType::Text | CoreFieldType::Keyword | CoreFieldType::Date => {
            Ok(JsonValue::String(value.to_string()))
        }
        CoreFieldType::I64 => value
            .parse::<i64>()
            .map(JsonValue::from)
            .map_err(|e| SearchError::InvalidQuery(format!("expected an integer: {e}"))),
        CoreFieldType::F64 => value
            .parse::<f64>()
            .map(JsonValue::from)
            .map_err(|e| SearchError::InvalidQuery(format!("expected a float: {e}"))),
        CoreFieldType::Bool => value
            .parse::<bool>()
            .map(JsonValue::from)
            .map_err(|e| SearchError::InvalidQuery(format!("expected a bool: {e}"))),
    }
}

/// Converts a core [`Document`] into an Elasticsearch `_source` body,
/// assigning it an id first if it didn't already have one - matching the
/// other backends' convention of generating one client-side rather than
/// deferring to the engine, so the id is known to the caller immediately.
pub fn document_to_source(document: Document) -> (String, JsonValue) {
    let id = document
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    (id, JsonValue::Object(document.fields))
}

/// Converts an Elasticsearch hit's `_id`/`_source` back into a core
/// [`Document`].
pub fn source_to_document(id: String, source: JsonValue) -> Document {
    let fields = match source {
        JsonValue::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    Document {
        id: Some(id),
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coerce_term_value_parses_numeric_and_bool_types() {
        assert_eq!(
            coerce_term_value(CoreFieldType::I64, "42").unwrap(),
            JsonValue::from(42)
        );
        assert_eq!(
            coerce_term_value(CoreFieldType::F64, "1.5").unwrap(),
            JsonValue::from(1.5)
        );
        assert_eq!(
            coerce_term_value(CoreFieldType::Bool, "true").unwrap(),
            JsonValue::from(true)
        );
        assert!(coerce_term_value(CoreFieldType::I64, "not-a-number").is_err());
    }

    #[test]
    fn coerce_term_value_passes_text_through_as_a_string() {
        assert_eq!(
            coerce_term_value(CoreFieldType::Keyword, "published").unwrap(),
            JsonValue::String("published".to_string())
        );
    }

    #[test]
    fn document_to_source_generates_an_id_when_missing() {
        let doc = Document::new().set("title", "no id yet");
        let (id, source) = document_to_source(doc);
        assert!(!id.is_empty());
        assert_eq!(source["title"], "no id yet");
    }

    #[test]
    fn document_to_source_keeps_an_existing_id() {
        let doc = Document::new().with_id("7").set("title", "has id");
        let (id, _) = document_to_source(doc);
        assert_eq!(id, "7");
    }

    #[test]
    fn source_to_document_roundtrips_fields() {
        let source = serde_json::json!({ "title": "hello", "views": 3 });
        let doc = source_to_document("1".to_string(), source);
        assert_eq!(doc.id.as_deref(), Some("1"));
        assert_eq!(
            doc.get("title"),
            Some(&JsonValue::String("hello".to_string()))
        );
    }
}
