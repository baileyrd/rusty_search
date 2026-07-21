use rusty_search_core::Document;
use serde_json::Value as JsonValue;

/// Algolia's reserved unique-identifier field, analogous to Meilisearch's
/// `"id"`/Solr's `"id"` - stored inside the record body rather than as
/// separate metadata the way Elasticsearch's `_id` is.
pub const OBJECT_ID: &str = "objectID";

/// Converts a core [`Document`] into the JSON object Algolia's batch API
/// expects, assigning it an id first if it didn't already have one -
/// matching the other backends' convention of generating one client-side.
pub fn document_to_json(document: Document) -> (String, JsonValue) {
    let id = document
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut fields = document.fields;
    fields.insert(OBJECT_ID.to_string(), JsonValue::String(id.clone()));
    (id, JsonValue::Object(fields))
}

/// Converts an Algolia hit (as returned by `/query`) back into a core
/// [`Document`], pulling `"objectID"` out into [`Document::id`] and
/// dropping Algolia's own ranking metadata fields, which aren't part of
/// the record.
pub fn json_to_document(value: JsonValue) -> Document {
    let mut fields = match value {
        JsonValue::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let id = fields
        .remove(OBJECT_ID)
        .and_then(|v| v.as_str().map(str::to_string));
    for meta_key in ["_highlightResult", "_snippetResult", "_rankingInfo"] {
        fields.remove(meta_key);
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
        assert_eq!(json["objectID"], id);
        assert_eq!(json["title"], "no id yet");
    }

    #[test]
    fn document_to_json_keeps_an_existing_id() {
        let doc = Document::new().with_id("7").set("title", "has id");
        let (id, json) = document_to_json(doc);
        assert_eq!(id, "7");
        assert_eq!(json["objectID"], "7");
    }

    #[test]
    fn json_to_document_pulls_object_id_and_metadata_out_of_fields() {
        let value = serde_json::json!({
            "objectID": "1",
            "title": "hello",
            "_highlightResult": { "title": {} }
        });
        let doc = json_to_document(value);
        assert_eq!(doc.id.as_deref(), Some("1"));
        assert!(doc.get("objectID").is_none());
        assert!(doc.get("_highlightResult").is_none());
        assert_eq!(
            doc.get("title"),
            Some(&JsonValue::String("hello".to_string()))
        );
    }
}
