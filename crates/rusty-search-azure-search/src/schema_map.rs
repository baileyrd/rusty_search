use std::collections::HashMap;

use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};
use serde_json::{json, Value};

/// The fixed name this crate gives every index's mandatory key field.
/// Azure AI Search requires exactly one key field per index; rather than
/// exposing its name as configurable, `rusty_search`'s `Document::id`
/// always maps onto a field named `"id"`, matching every other backend's
/// convention of a single, well-known id field.
///
/// Azure restricts key values to letters, digits, underscore, dash, and
/// equal sign - this crate doesn't validate that client-side, so a
/// caller-supplied id outside that set surfaces as a 400 from Azure itself
/// (see the crate's module docs).
pub const KEY_FIELD: &str = "id";

/// Per-field metadata kept alongside the fields we sent Azure, so query and
/// sort translation know how to treat a field without a round trip back to
/// the index definition.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field_type: CoreFieldType,
    /// Whether this field was created with `sortable: true`. Azure AI
    /// Search, like `rusty-search-tantivy`'s "fast fields", requires a
    /// field to be marked sortable at index-creation time before it can be
    /// used in a native `$orderby` clause - unlike Elasticsearch, where
    /// every sortable core field type is already sortable by default.
    pub sortable: bool,
}

pub type FieldMap = HashMap<String, FieldMeta>;

/// Translates a core [`CoreSchema`] into the `fields` array of an Azure AI
/// Search index-creation body (the caller still has to add `"name"`, since
/// `CoreSchema` doesn't carry the index name), alongside the field-type map
/// used later for query/sort translation.
///
/// `FieldOptions::fast` maps onto Azure's `sortable` attribute - the one
/// core option every other remote backend in this workspace has ignored so
/// far, since Azure is the first one where sorting genuinely does require a
/// dedicated, upfront-declared representation. `FieldOptions::stored` maps
/// onto `retrievable`. Text fields are never `filterable`/`sortable`, the
/// same restriction `rusty-search-tantivy` applies to fast fields.
pub fn build_index_body(schema: &CoreSchema) -> (Value, FieldMap) {
    let mut fields = FieldMap::new();
    let mut field_defs = vec![json!({
        "name": KEY_FIELD,
        "type": "Edm.String",
        "key": true,
        "searchable": false,
        "filterable": true,
        "sortable": false,
        "facetable": false,
        "retrievable": true,
    })];

    for def in &schema.fields {
        let (edm_type, is_text) = match def.field_type {
            CoreFieldType::Text => ("Edm.String", true),
            CoreFieldType::Keyword => ("Edm.String", false),
            CoreFieldType::I64 => ("Edm.Int64", false),
            CoreFieldType::F64 => ("Edm.Double", false),
            CoreFieldType::Bool => ("Edm.Boolean", false),
            CoreFieldType::Date => ("Edm.DateTimeOffset", false),
        };

        let searchable = is_text && def.options.indexed;
        let filterable = !is_text && def.options.indexed;
        let sortable = !is_text && def.options.fast;

        field_defs.push(json!({
            "name": def.name,
            "type": edm_type,
            "searchable": searchable,
            "filterable": filterable,
            "sortable": sortable,
            "facetable": filterable,
            "retrievable": def.options.stored,
        }));
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field_type: def.field_type,
                sortable,
            },
        );
    }

    (json!({ "fields": field_defs }), fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Schema};

    fn field<'a>(body: &'a Value, name: &str) -> &'a Value {
        body["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .unwrap_or_else(|| panic!("field `{name}` not present"))
    }

    #[test]
    fn always_includes_a_key_field_named_id() {
        let schema = Schema::builder().text("title").build();
        let (body, _) = build_index_body(&schema);
        let id_field = field(&body, "id");
        assert_eq!(id_field["type"], "Edm.String");
        assert_eq!(id_field["key"], true);
    }

    #[test]
    fn maps_each_core_field_type_to_its_edm_type() {
        let schema = Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .f64_field("rating")
            .bool_field("published")
            .date_field("created_at")
            .build();
        let (body, fields) = build_index_body(&schema);

        assert_eq!(field(&body, "title")["type"], "Edm.String");
        assert_eq!(field(&body, "status")["type"], "Edm.String");
        assert_eq!(field(&body, "views")["type"], "Edm.Int64");
        assert_eq!(field(&body, "rating")["type"], "Edm.Double");
        assert_eq!(field(&body, "published")["type"], "Edm.Boolean");
        assert_eq!(field(&body, "created_at")["type"], "Edm.DateTimeOffset");
        assert_eq!(fields["views"].field_type, CoreFieldType::I64);
    }

    #[test]
    fn text_fields_are_searchable_not_filterable_or_sortable() {
        let schema = Schema::builder()
            .text_with("title", FieldOptions::new().fast(true))
            .build();
        let (body, fields) = build_index_body(&schema);
        let title = field(&body, "title");
        assert_eq!(title["searchable"], true);
        assert_eq!(title["filterable"], false);
        assert_eq!(title["sortable"], false);
        assert!(!fields["title"].sortable);
    }

    #[test]
    fn non_text_fields_are_filterable_not_searchable() {
        let schema = Schema::builder().keyword("status").build();
        let (body, _) = build_index_body(&schema);
        let status = field(&body, "status");
        assert_eq!(status["searchable"], false);
        assert_eq!(status["filterable"], true);
        assert_eq!(status["facetable"], true);
    }

    #[test]
    fn fast_option_becomes_sortable_true() {
        let schema = Schema::builder()
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build();
        let (body, fields) = build_index_body(&schema);
        assert_eq!(field(&body, "views")["sortable"], true);
        assert!(fields["views"].sortable);
    }

    #[test]
    fn fields_without_fast_are_not_sortable() {
        let schema = Schema::builder().i64_field("views").build();
        let (body, fields) = build_index_body(&schema);
        assert_eq!(field(&body, "views")["sortable"], false);
        assert!(!fields["views"].sortable);
    }

    #[test]
    fn unstored_field_is_not_retrievable() {
        let schema = Schema::builder()
            .text_with("internal_notes", FieldOptions::new().stored(false))
            .build();
        let (body, _) = build_index_body(&schema);
        assert_eq!(field(&body, "internal_notes")["retrievable"], false);
    }
}
