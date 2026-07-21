use std::collections::HashMap;

use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};
use serde_json::{json, Value};

/// Per-field metadata kept alongside the settings we sent Algolia, so
/// query translation knows how to render a `Query`'s string/JSON values as
/// the right filter-expression literal. Algolia itself has no field-type
/// system - it infers a value's type from the JSON on each record - so
/// this map exists purely for *our* side of the translation.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field_type: CoreFieldType,
}

pub type FieldMap = HashMap<String, FieldMeta>;

/// Translates a core [`CoreSchema`] into Algolia index settings
/// (`searchableAttributes`/`attributesForFaceting`), alongside the
/// field-type map used later for query translation.
///
/// `Text` fields become searchable attributes. Every other field type
/// becomes a `filterOnly(...)` faceting attribute, since
/// [`Query::Term`](rusty_search_core::Query::Term)/
/// [`Query::Range`](rusty_search_core::Query::Range) only need filtering,
/// not Algolia's facet-count UI. `FieldOptions::stored`/`indexed`/`fast`
/// have no analog in Algolia's model (every attribute is stored and
/// returned by default, and there's no per-field "fast" concept the way
/// Tantivy/Elasticsearch/Solr have one) and are ignored here.
pub fn build_settings(schema: &CoreSchema) -> (Value, FieldMap) {
    let mut searchable = Vec::new();
    let mut faceting = Vec::new();
    let mut fields = FieldMap::new();

    for def in &schema.fields {
        match def.field_type {
            CoreFieldType::Text => searchable.push(def.name.clone()),
            CoreFieldType::Keyword
            | CoreFieldType::I64
            | CoreFieldType::F64
            | CoreFieldType::Bool
            | CoreFieldType::Date => faceting.push(format!("filterOnly({})", def.name)),
        }
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field_type: def.field_type,
            },
        );
    }

    let settings = json!({
        "searchableAttributes": searchable,
        "attributesForFaceting": faceting,
    });
    (settings, fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::Schema;

    #[test]
    fn text_fields_are_searchable_others_are_filter_only() {
        let schema = Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .build();
        let (settings, fields) = build_settings(&schema);

        assert_eq!(settings["searchableAttributes"], json!(["title"]));
        assert_eq!(
            settings["attributesForFaceting"],
            json!(["filterOnly(status)", "filterOnly(views)"])
        );
        assert_eq!(fields["views"].field_type, CoreFieldType::I64);
    }
}
