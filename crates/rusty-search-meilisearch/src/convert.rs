use rusty_search_core::Document;
use serde_json::Value as JsonValue;

/// Meilisearch stores a document's primary key *inside* the document body
/// (unlike Elasticsearch's separate `_id`/`_source`, or Tantivy's reserved
/// field), so this backend always uses `"id"` as the primary key and keeps
/// it out of `Document::fields` on the way back, mirroring how the other
/// backends keep their own id representation out of `fields`.
pub const PRIMARY_KEY: &str = "id";

/// Converts a core [`Document`] into the JSON object Meilisearch expects,
/// assigning it an id first if it didn't already have one - matching the
/// other backends' convention of generating one client-side.
pub fn document_to_json(document: Document) -> (String, JsonValue) {
    let id = document
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut fields = document.fields;
    fields.insert(PRIMARY_KEY.to_string(), JsonValue::String(id.clone()));
    (id, JsonValue::Object(fields))
}

/// Converts a Meilisearch document (as returned by search or document
/// fetch) back into a core [`Document`], pulling `"id"` out into
/// [`Document::id`].
pub fn json_to_document(value: JsonValue) -> Document {
    let mut fields = match value {
        JsonValue::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    let id = fields
        .remove(PRIMARY_KEY)
        .and_then(|v| v.as_str().map(str::to_string));
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
    fn json_to_document_pulls_id_out_of_fields() {
        let value = serde_json::json!({ "id": "1", "title": "hello" });
        let doc = json_to_document(value);
        assert_eq!(doc.id.as_deref(), Some("1"));
        assert!(doc.get("id").is_none());
        assert_eq!(
            doc.get("title"),
            Some(&JsonValue::String("hello".to_string()))
        );
    }
}
