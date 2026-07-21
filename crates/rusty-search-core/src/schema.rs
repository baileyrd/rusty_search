use serde::{Deserialize, Serialize};

/// The data type of a schema field.
///
/// Backends map these onto their own native field types (e.g. `Text` maps
/// onto a tokenized/analyzed field, `Keyword` onto an untokenized exact-match
/// field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    /// Free text, analyzed/tokenized for full-text search.
    Text,
    /// A string matched exactly, not tokenized (ids, statuses, tags).
    Keyword,
    /// A 64-bit signed integer.
    I64,
    /// A 64-bit float.
    F64,
    /// A boolean.
    Bool,
    /// An RFC 3339 timestamp string.
    Date,
}

/// Per-field storage/indexing behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOptions {
    /// Whether the field's original value is stored and returned in search
    /// hits. Disable for large fields you don't need back verbatim.
    pub stored: bool,
    /// Whether the field can be searched/filtered on.
    pub indexed: bool,
    /// Whether the field supports fast sorting/range queries. Backends that
    /// need a separate columnar representation (e.g. Tantivy's "fast
    /// fields") use this to decide whether to build one.
    pub fast: bool,
}

impl Default for FieldOptions {
    fn default() -> Self {
        Self {
            stored: true,
            indexed: true,
            fast: false,
        }
    }
}

impl FieldOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stored(mut self, stored: bool) -> Self {
        self.stored = stored;
        self
    }

    pub fn indexed(mut self, indexed: bool) -> Self {
        self.indexed = indexed;
        self
    }

    pub fn fast(mut self, fast: bool) -> Self {
        self.fast = fast;
        self
    }
}

/// A single named field in a [`Schema`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: String,
    pub field_type: FieldType,
    pub options: FieldOptions,
}

/// An index's field layout, provided to a backend on index creation.
///
/// Built with [`Schema::builder`], following the same fluent style as the
/// query DSL:
///
/// ```
/// use rusty_search_core::{Schema, FieldOptions};
///
/// let schema = Schema::builder()
///     .text("title")
///     .text("body")
///     .keyword("status")
///     .i64_field_with("views", FieldOptions::new().fast(true))
///     .build();
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub fields: Vec<FieldDefinition>,
}

impl Schema {
    pub fn builder() -> SchemaBuilder {
        SchemaBuilder::new()
    }

    pub fn field(&self, name: &str) -> Option<&FieldDefinition> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// Fluent builder for [`Schema`].
#[derive(Debug, Clone, Default)]
pub struct SchemaBuilder {
    fields: Vec<FieldDefinition>,
}

macro_rules! field_methods {
    ($name:ident, $with:ident, $ty:expr) => {
        pub fn $name(self, name: impl Into<String>) -> Self {
            self.$with(name, FieldOptions::default())
        }

        pub fn $with(mut self, name: impl Into<String>, options: FieldOptions) -> Self {
            self.fields.push(FieldDefinition {
                name: name.into(),
                field_type: $ty,
                options,
            });
            self
        }
    };
}

impl SchemaBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    field_methods!(text, text_with, FieldType::Text);
    field_methods!(keyword, keyword_with, FieldType::Keyword);
    field_methods!(i64_field, i64_field_with, FieldType::I64);
    field_methods!(f64_field, f64_field_with, FieldType::F64);
    field_methods!(bool_field, bool_field_with, FieldType::Bool);
    field_methods!(date_field, date_field_with, FieldType::Date);

    pub fn build(self) -> Schema {
        Schema {
            fields: self.fields,
        }
    }
}
