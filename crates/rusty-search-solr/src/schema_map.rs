use std::collections::HashMap;

use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};
use serde_json::{json, Value};

/// Per-field metadata kept alongside the fields we added via Solr's Schema
/// API, so query translation knows how to render a `Query`'s string/JSON
/// values as the right Lucene query-string literal (a quoted phrase vs. a
/// bare number/boolean) without a round trip to fetch the schema back.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field_type: CoreFieldType,
}

pub type FieldMap = HashMap<String, FieldMeta>;

/// Translates a core [`CoreSchema`] into a Solr
/// [Schema API](https://solr.apache.org/guide/solr/latest/indexing-guide/schema-api.html)
/// `add-field` request body, alongside the field-type map used later for
/// query/sort translation.
///
/// Solr's `_default` configset already defines `id` as the unique-key
/// field, matching this workspace's `Document::id` convention, so it isn't
/// added here. `FieldOptions::fast` maps to Solr's `docValues` (its
/// equivalent of a "fast field") for every field type except `Text` -
/// Solr's tokenized `text_general` field type doesn't support docValues,
/// so `fast` is silently ignored there rather than producing a schema
/// error.
pub fn build_add_field_body(schema: &CoreSchema) -> (Value, FieldMap) {
    let mut field_defs = Vec::new();
    let mut fields = FieldMap::new();

    for def in &schema.fields {
        let solr_type = match def.field_type {
            CoreFieldType::Text => "text_general",
            CoreFieldType::Keyword => "string",
            CoreFieldType::I64 => "plong",
            CoreFieldType::F64 => "pdouble",
            CoreFieldType::Bool => "boolean",
            CoreFieldType::Date => "pdate",
        };
        let doc_values = def.options.fast && !matches!(def.field_type, CoreFieldType::Text);

        field_defs.push(json!({
            "name": def.name,
            "type": solr_type,
            "stored": def.options.stored,
            "indexed": def.options.indexed,
            "docValues": doc_values,
            "multiValued": false,
        }));
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field_type: def.field_type,
            },
        );
    }

    (json!({ "add-field": field_defs }), fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Schema};

    #[test]
    fn maps_each_core_field_type_to_a_solr_field_type() {
        let schema = Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .f64_field("rating")
            .bool_field("published")
            .date_field("created_at")
            .build();
        let (body, fields) = build_add_field_body(&schema);

        let by_name = |name: &str| -> &Value {
            body["add-field"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap()
        };

        assert_eq!(by_name("title")["type"], "text_general");
        assert_eq!(by_name("status")["type"], "string");
        assert_eq!(by_name("views")["type"], "plong");
        assert_eq!(by_name("rating")["type"], "pdouble");
        assert_eq!(by_name("published")["type"], "boolean");
        assert_eq!(by_name("created_at")["type"], "pdate");
        assert_eq!(fields["views"].field_type, CoreFieldType::I64);
    }

    #[test]
    fn fast_maps_to_doc_values_except_for_text_fields() {
        let schema = Schema::builder()
            .text_with("title", FieldOptions::new().fast(true))
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build();
        let (body, _) = build_add_field_body(&schema);

        let by_name = |name: &str| -> &Value {
            body["add-field"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == name)
                .unwrap()
        };
        assert_eq!(by_name("title")["docValues"], false);
        assert_eq!(by_name("views")["docValues"], true);
    }
}
