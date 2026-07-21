use std::collections::HashMap;

use rusty_search_core::{Document, FieldType as CoreFieldType, SearchError};
use serde_json::Value as JsonValue;
use tantivy::schema::document::{Document as TantivyDocumentTrait, TantivyDocument};
use tantivy::schema::{Field, OwnedValue, Schema as TantivySchema};
use tantivy::Term;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::schema_map::{FieldMeta, ID_FIELD_NAME};

/// Parses an RFC 3339 timestamp string into a Tantivy `DateTime`.
pub fn parse_date(value: &str) -> Result<tantivy::DateTime, SearchError> {
    let dt = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|e| SearchError::InvalidQuery(format!("invalid RFC 3339 date `{value}`: {e}")))?;
    Ok(tantivy::DateTime::from_utc(dt))
}

/// Builds a `Term` for exact matching (used by `Query::Term` and range
/// bounds) against `field`, converting the JSON-ish string representation
/// callers pass in the core `Query` DSL into the field's native type.
pub fn value_to_term(
    field: Field,
    field_type: CoreFieldType,
    value: &str,
) -> Result<Term, SearchError> {
    match field_type {
        CoreFieldType::Text | CoreFieldType::Keyword => Ok(Term::from_field_text(field, value)),
        CoreFieldType::I64 => value
            .parse::<i64>()
            .map(|v| Term::from_field_i64(field, v))
            .map_err(|e| SearchError::InvalidQuery(format!("expected an integer: {e}"))),
        CoreFieldType::F64 => value
            .parse::<f64>()
            .map(|v| Term::from_field_f64(field, v))
            .map_err(|e| SearchError::InvalidQuery(format!("expected a float: {e}"))),
        CoreFieldType::Bool => value
            .parse::<bool>()
            .map(|v| Term::from_field_bool(field, v))
            .map_err(|e| SearchError::InvalidQuery(format!("expected a bool: {e}"))),
        CoreFieldType::Date => parse_date(value).map(|dt| Term::from_field_date(field, dt)),
    }
}

/// Builds a `Term` for a numeric/date range bound expressed as a JSON value
/// (as carried by `Query::Range`).
pub fn json_value_to_term(
    field: Field,
    field_type: CoreFieldType,
    value: &JsonValue,
) -> Result<Term, SearchError> {
    match field_type {
        CoreFieldType::I64 => value
            .as_i64()
            .map(|v| Term::from_field_i64(field, v))
            .ok_or_else(|| SearchError::InvalidQuery("expected an integer".to_string())),
        CoreFieldType::F64 => value
            .as_f64()
            .map(|v| Term::from_field_f64(field, v))
            .ok_or_else(|| SearchError::InvalidQuery("expected a number".to_string())),
        CoreFieldType::Date => {
            let s = value.as_str().ok_or_else(|| {
                SearchError::InvalidQuery("expected an RFC 3339 date string".to_string())
            })?;
            parse_date(s).map(|dt| Term::from_field_date(field, dt))
        }
        other => Err(SearchError::InvalidQuery(format!(
            "range queries are not supported on {other:?} fields"
        ))),
    }
}

/// Converts a core [`Document`] into a Tantivy document ready for indexing,
/// assigning it an id first if it didn't already have one.
///
/// Fields not present in the index's schema are silently dropped, matching
/// `TantivyDocument::from_json_object`'s own behavior.
pub fn document_to_tantivy(
    tantivy_schema: &TantivySchema,
    document: Document,
) -> (String, TantivyDocument) {
    let id = document
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let mut object = document.fields;
    object.insert(ID_FIELD_NAME.to_string(), JsonValue::String(id.clone()));

    let tantivy_doc = TantivyDocument::from_json_object(tantivy_schema, object)
        .expect("document fields were already validated against this schema");
    (id, tantivy_doc)
}

/// Converts a Tantivy document (as retrieved from a `Searcher`) back into a
/// core [`Document`], pulling the reserved id field out into `Document::id`.
pub fn tantivy_doc_to_document(
    tantivy_doc: &TantivyDocument,
    tantivy_schema: &TantivySchema,
    fields: &HashMap<String, FieldMeta>,
) -> Document {
    let named = tantivy_doc.to_named_doc(tantivy_schema);
    let mut id = None;
    let mut object = serde_json::Map::new();

    for (name, values) in named.0 {
        if name == ID_FIELD_NAME {
            if let Some(OwnedValue::Str(s)) = values.into_iter().next() {
                id = Some(s);
            }
            continue;
        }
        if !fields.contains_key(&name) {
            continue;
        }
        let mut json_values: Vec<JsonValue> = values
            .into_iter()
            .map(|v| serde_json::to_value(v).unwrap_or(JsonValue::Null))
            .collect();
        let value = if json_values.len() == 1 {
            json_values.pop().unwrap()
        } else {
            JsonValue::Array(json_values)
        };
        object.insert(name, value);
    }

    Document { id, fields: object }
}
