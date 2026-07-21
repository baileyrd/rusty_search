use std::collections::HashMap;

use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};
use serde_json::{json, Value};

/// Per-field metadata kept alongside the mapping we sent Elasticsearch, so
/// query translation knows how to coerce a `Query`'s string/JSON values into
/// the field's real type without a round trip to fetch the mapping back.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field_type: CoreFieldType,
}

pub type FieldMap = HashMap<String, FieldMeta>;

/// Translates a core [`CoreSchema`] into an Elasticsearch index-creation
/// body (`{"mappings": {"properties": {...}}}`), alongside the field-type
/// map used later for query/sort translation.
///
/// `FieldOptions::fast` has no counterpart here: Elasticsearch already
/// keeps doc_values (its equivalent of a "fast field") for `keyword`,
/// numeric, boolean, and `date` fields by default, so every sortable core
/// field type is sortable in Elasticsearch out of the box. `stored` isn't
/// mapped either - Elasticsearch's `_source` already stores the original
/// document verbatim, which is what `Document` round-trips through; opting
/// a field out of `_source` is a index-wide/field-exclusion concern this
/// crate doesn't expose.
pub fn build_index_body(schema: &CoreSchema) -> (Value, FieldMap) {
    let mut properties = serde_json::Map::new();
    let mut fields = FieldMap::new();

    for def in &schema.fields {
        let es_type = match def.field_type {
            CoreFieldType::Text => "text",
            CoreFieldType::Keyword => "keyword",
            CoreFieldType::I64 => "long",
            CoreFieldType::F64 => "double",
            CoreFieldType::Bool => "boolean",
            CoreFieldType::Date => "date",
        };
        let mut property = json!({ "type": es_type });
        if !def.options.indexed {
            property["index"] = json!(false);
        }
        properties.insert(def.name.clone(), property);
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field_type: def.field_type,
            },
        );
    }

    let body = json!({ "mappings": { "properties": Value::Object(properties) } });
    (body, fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Schema};

    #[test]
    fn maps_each_core_field_type_to_its_es_type() {
        let schema = Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .f64_field("rating")
            .bool_field("published")
            .date_field("created_at")
            .build();

        let (body, fields) = build_index_body(&schema);
        let properties = &body["mappings"]["properties"];

        assert_eq!(properties["title"]["type"], "text");
        assert_eq!(properties["status"]["type"], "keyword");
        assert_eq!(properties["views"]["type"], "long");
        assert_eq!(properties["rating"]["type"], "double");
        assert_eq!(properties["published"]["type"], "boolean");
        assert_eq!(properties["created_at"]["type"], "date");

        assert_eq!(fields["views"].field_type, CoreFieldType::I64);
    }

    #[test]
    fn non_indexed_field_gets_index_false() {
        let schema = Schema::builder()
            .text_with("internal_notes", FieldOptions::new().indexed(false))
            .build();
        let (body, _) = build_index_body(&schema);
        assert_eq!(
            body["mappings"]["properties"]["internal_notes"]["index"],
            false
        );
    }

    #[test]
    fn indexed_field_has_no_index_key() {
        let schema = Schema::builder().text("title").build();
        let (body, _) = build_index_body(&schema);
        assert!(body["mappings"]["properties"]["title"]
            .get("index")
            .is_none());
    }
}
