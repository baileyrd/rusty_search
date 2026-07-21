use std::collections::HashMap;

use rusty_search_core::{FieldType as CoreFieldType, Schema as CoreSchema};
use tantivy::schema::{
    DateOptions, Field, NumericOptions, Schema as TantivySchema, SchemaBuilder, FAST, STORED,
    STRING, TEXT,
};

/// Per-field metadata we keep alongside Tantivy's own schema, so query/sort
/// translation knows how to build `Term`s and pick a sort strategy for a
/// given core field name without re-deriving it from Tantivy's schema.
#[derive(Debug, Clone, Copy)]
pub struct FieldMeta {
    pub field: Field,
    pub field_type: CoreFieldType,
    pub fast: bool,
}

/// The reserved field every index gets, holding the document id as an exact
/// (untokenized) string.
pub const ID_FIELD_NAME: &str = "_id";

pub struct MappedSchema {
    pub tantivy_schema: TantivySchema,
    pub fields: HashMap<String, FieldMeta>,
    pub id_field: Field,
}

/// Translates a backend-agnostic [`CoreSchema`] into a Tantivy `Schema`,
/// recording enough metadata to translate queries and sorts back and forth
/// later.
pub fn build_tantivy_schema(schema: &CoreSchema) -> MappedSchema {
    let mut builder = SchemaBuilder::new();
    let id_field = builder.add_text_field(ID_FIELD_NAME, STRING | STORED);

    let mut fields = HashMap::new();
    for def in &schema.fields {
        let field = match def.field_type {
            CoreFieldType::Text => {
                let mut opts = TEXT;
                if def.options.stored {
                    opts = opts | STORED;
                }
                builder.add_text_field(&def.name, opts)
            }
            CoreFieldType::Keyword => {
                let mut opts = STRING;
                if def.options.stored {
                    opts = opts | STORED;
                }
                if def.options.fast {
                    opts = opts | FAST;
                }
                builder.add_text_field(&def.name, opts)
            }
            CoreFieldType::I64 => builder.add_i64_field(&def.name, numeric_options(def)),
            CoreFieldType::F64 => builder.add_f64_field(&def.name, numeric_options(def)),
            CoreFieldType::Bool => builder.add_bool_field(&def.name, numeric_options(def)),
            CoreFieldType::Date => builder.add_date_field(&def.name, date_options(def)),
        };
        fields.insert(
            def.name.clone(),
            FieldMeta {
                field,
                field_type: def.field_type,
                fast: def.options.fast,
            },
        );
    }

    MappedSchema {
        tantivy_schema: builder.build(),
        fields,
        id_field,
    }
}

fn numeric_options(def: &rusty_search_core::FieldDefinition) -> NumericOptions {
    let mut opts = NumericOptions::default();
    if def.options.indexed {
        opts = opts.set_indexed();
    }
    if def.options.stored {
        opts = opts.set_stored();
    }
    if def.options.fast {
        opts = opts.set_fast();
    }
    opts
}

fn date_options(def: &rusty_search_core::FieldDefinition) -> DateOptions {
    let mut opts = DateOptions::default();
    if def.options.indexed {
        opts = opts.set_indexed();
    }
    if def.options.stored {
        opts = opts.set_stored();
    }
    if def.options.fast {
        opts = opts.set_fast();
    }
    opts
}
