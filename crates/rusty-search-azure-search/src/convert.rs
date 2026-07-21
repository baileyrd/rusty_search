use rusty_search_core::Document;
use serde_json::Value as JsonValue;

use crate::schema_map::KEY_FIELD;

/// Metadata keys Azure AI Search adds to every search hit alongside the
/// document's own fields, stripped back out when converting a hit into a
/// core [`Document`].
const METADATA_KEYS: [&str; 3] = [
    "@search.score",
    "@search.highlights",
    "@search.rerankerScore",
];

/// Converts a core [`Document`] into an Azure AI Search document body,
/// assigning it an id first if it didn't already have one (matching the
/// other remote backends' convention), keyed under the fixed field name
/// `"id"` - see [`KEY_FIELD`]'s docs for why that's fixed rather than
/// configurable.
pub fn document_to_json(document: Document) -> (String, JsonValue) {
    let id = document
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut fields = document.fields;
    fields.insert(KEY_FIELD.to_string(), JsonValue::String(id.clone()));
    (id, JsonValue::Object(fields))
}

/// Converts an Azure AI Search document (as returned in a search hit) back
/// into a core [`Document`], stripping the `@search.*` metadata fields
/// Azure adds alongside the document's own fields.
pub fn json_to_document(value: JsonValue) -> Document {
    let mut fields = match value {
        JsonValue::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let id = fields
        .remove(KEY_FIELD)
        .and_then(|v| v.as_str().map(str::to_string));
    for key in METADATA_KEYS {
        fields.remove(key);
    }
    Document { id, fields }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_to_json_generates_an_id_when_missing() {
        let doc = Document::new().set("title", "no id yet");
        let (id, json) = document_to_json(doc);
        assert!(!id.is_empty());
        assert_eq!(json["id"], id);
        assert_eq!(json["title"], "no id yet");
    }

    #[test]
    fn document_to_json_keeps_an_existing_id() {
        let doc = Document::new().with_id("7").set("title", "has id");
        let (id, json) = document_to_json(doc);
        assert_eq!(id, "7");
        assert_eq!(json["id"], "7");
    }

    #[test]
    fn json_to_document_strips_search_metadata() {
        let value = serde_json::json!({
            "id": "1",
            "title": "hello",
            "@search.score": 1.23,
            "@search.highlights": {},
        });
        let doc = json_to_document(value);
        assert_eq!(doc.id.as_deref(), Some("1"));
        assert_eq!(
            doc.get("title"),
            Some(&JsonValue::String("hello".to_string()))
        );
        assert!(doc.get("@search.score").is_none());
        assert!(doc.get("@search.highlights").is_none());
    }
}
