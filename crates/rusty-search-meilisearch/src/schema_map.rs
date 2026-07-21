use std::collections::HashMap;

use meilisearch_sdk::settings::Settings;
use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};

/// Per-field metadata kept alongside the settings we sent Meilisearch, so
/// query translation knows how to render a `Query`'s string/JSON values as
/// the right filter-expression literal (a quoted string vs. a bare number
/// or boolean) without a round trip to fetch the settings back.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field_type: CoreFieldType,
}

pub type FieldMap = HashMap<String, FieldMeta>;

/// Translates a core [`CoreSchema`] into Meilisearch index settings,
/// alongside the field-type map used later for query/sort translation.
///
/// - `Text` fields become searchable attributes (Meilisearch's full-text,
///   typo-tolerant matching).
/// - `Keyword`/`I64`/`F64`/`Bool`/`Date` fields become filterable
///   attributes, since [`Query::Term`](rusty_search_core::Query::Term) and
///   [`Query::Range`](rusty_search_core::Query::Range) both compile down to
///   Meilisearch filter expressions, which require the target field to be
///   filterable.
/// - Fields with `FieldOptions::fast` also become sortable attributes -
///   Meilisearch's equivalent of a "fast field" for sort/range performance.
pub fn build_settings_and_fields(schema: &CoreSchema) -> (Settings, FieldMap) {
    let mut searchable = Vec::new();
    let mut filterable = Vec::new();
    let mut sortable = Vec::new();
    let mut fields = FieldMap::new();

    for def in &schema.fields {
        match def.field_type {
            CoreFieldType::Text => searchable.push(def.name.clone()),
            CoreFieldType::Keyword
            | CoreFieldType::I64
            | CoreFieldType::F64
            | CoreFieldType::Bool
            | CoreFieldType::Date => filterable.push(def.name.clone()),
        }
        if def.options.fast {
            sortable.push(def.name.clone());
        }
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field_type: def.field_type,
            },
        );
    }

    let settings = Settings::new()
        .with_searchable_attributes(searchable)
        .with_filterable_attributes(filterable)
        .with_sortable_attributes(sortable);

    (settings, fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Schema};

    #[test]
    fn text_fields_are_searchable_others_are_filterable() {
        let schema = Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .build();
        let (settings, fields) = build_settings_and_fields(&schema);

        assert_eq!(
            settings.searchable_attributes.as_deref(),
            Some(&["title".to_string()][..])
        );
        let filterable = settings.filterable_attributes.unwrap();
        assert_eq!(filterable.len(), 2);
        assert_eq!(fields["views"].field_type, CoreFieldType::I64);
    }

    #[test]
    fn only_fast_fields_are_sortable() {
        let schema = Schema::builder()
            .i64_field("views")
            .i64_field_with("rank", FieldOptions::new().fast(true))
            .build();
        let (settings, _) = build_settings_and_fields(&schema);
        assert_eq!(
            settings.sortable_attributes.as_deref(),
            Some(&["rank".to_string()][..])
        );
    }
}
