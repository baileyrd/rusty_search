use rusty_search_core::{Query as CoreQuery, SearchError};
use serde_json::{json, Map, Value};

use crate::convert::coerce_term_value;
use crate::schema_map::{FieldMap, FieldMeta};

/// Translates a core [`CoreQuery`] into an Elasticsearch Query DSL body.
///
/// `Query::Bool`'s `must`/`should`/`must_not`/`filter` map onto
/// Elasticsearch's `bool` query one-for-one - including `filter` being a
/// genuinely non-scoring clause, since that's exactly what
/// [`CoreQuery::Bool`]'s own doc comment already specifies. Every other
/// `SearchBackend` in this workspace has to approximate that; this one
/// doesn't need to.
pub fn build_query(query: &CoreQuery, fields: &FieldMap) -> Result<Value, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(json!({ "match_all": {} })),

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            let coerced = coerce_term_value(meta.field_type, value)?;
            Ok(json!({ "term": single_field(field, coerced) }))
        }

        CoreQuery::Match { field, value } => {
            lookup(fields, field)?;
            Ok(json!({ "match": single_field(field, json!(value)) }))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            let mut bounds = Map::new();
            if let Some(v) = gte {
                bounds.insert("gte".to_string(), coerce_range_bound(meta, v)?);
            }
            if let Some(v) = lte {
                bounds.insert("lte".to_string(), coerce_range_bound(meta, v)?);
            }
            Ok(json!({ "range": single_field(field, Value::Object(bounds)) }))
        }

        CoreQuery::Bool {
            must,
            should,
            must_not,
            filter,
        } => Ok(json!({
            "bool": {
                "must": translate_all(must, fields)?,
                "should": translate_all(should, fields)?,
                "must_not": translate_all(must_not, fields)?,
                "filter": translate_all(filter, fields)?,
            }
        })),
    }
}

fn translate_all(queries: &[CoreQuery], fields: &FieldMap) -> Result<Vec<Value>, SearchError> {
    queries.iter().map(|q| build_query(q, fields)).collect()
}

fn single_field(name: &str, value: Value) -> Value {
    let mut object = Map::new();
    object.insert(name.to_string(), value);
    Value::Object(object)
}

fn coerce_range_bound(meta: &FieldMeta, value: &Value) -> Result<Value, SearchError> {
    use rusty_search_core::FieldType;
    let ok = match meta.field_type {
        FieldType::I64 => value.is_i64() || value.is_u64(),
        FieldType::F64 => value.is_number(),
        FieldType::Date => value.is_string(),
        FieldType::Text | FieldType::Keyword | FieldType::Bool => false,
    };
    if ok {
        Ok(value.clone())
    } else {
        Err(SearchError::InvalidQuery(format!(
            "range queries are not supported on {:?} fields",
            meta.field_type
        )))
    }
}

fn lookup<'a>(fields: &'a FieldMap, name: &str) -> Result<&'a FieldMeta, SearchError> {
    fields
        .get(name)
        .ok_or_else(|| SearchError::InvalidQuery(format!("unknown field `{name}`")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldType, Query};

    fn fields() -> FieldMap {
        [
            (
                "title".to_string(),
                FieldMeta {
                    field_type: FieldType::Text,
                },
            ),
            (
                "status".to_string(),
                FieldMeta {
                    field_type: FieldType::Keyword,
                },
            ),
            (
                "views".to_string(),
                FieldMeta {
                    field_type: FieldType::I64,
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn match_all_translates_directly() {
        assert_eq!(
            build_query(&Query::match_all(), &fields()).unwrap(),
            json!({ "match_all": {} })
        );
    }

    #[test]
    fn term_coerces_numeric_value() {
        let q = Query::term("views", "42");
        let translated = build_query(&q, &fields()).unwrap();
        assert_eq!(translated, json!({ "term": { "views": 42 } }));
    }

    #[test]
    fn term_on_unknown_field_errors() {
        let q = Query::term("nonexistent", "x");
        assert!(build_query(&q, &fields()).is_err());
    }

    #[test]
    fn match_translates_to_es_match() {
        let q = Query::match_query("title", "rust search");
        let translated = build_query(&q, &fields()).unwrap();
        assert_eq!(translated, json!({ "match": { "title": "rust search" } }));
    }

    #[test]
    fn range_only_includes_provided_bounds() {
        let q = Query::range("views", Some(10.into()), None);
        let translated = build_query(&q, &fields()).unwrap();
        assert_eq!(translated, json!({ "range": { "views": { "gte": 10 } } }));
    }

    #[test]
    fn range_on_keyword_field_errors() {
        let q = Query::range("status", Some("a".into()), None);
        assert!(build_query(&q, &fields()).is_err());
    }

    #[test]
    fn bool_keeps_filter_as_its_own_array_distinct_from_must() {
        let filtered = Query::Bool {
            must: vec![Query::match_query("title", "rust")],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::term("status", "published")],
        };
        let translated = build_query(&filtered, &fields()).unwrap();
        assert_eq!(
            translated["bool"]["filter"],
            json!([{ "term": { "status": "published" } }])
        );
        assert_eq!(
            translated["bool"]["must"],
            json!([{ "match": { "title": "rust" } }])
        );
        assert_eq!(translated["bool"]["should"], json!([]));
        assert_eq!(translated["bool"]["must_not"], json!([]));
    }

    #[test]
    fn not_wraps_in_must_not() {
        let q = Query::term("status", "published").not();
        let translated = build_query(&q, &fields()).unwrap();
        assert_eq!(
            translated["bool"]["must_not"],
            json!([{ "term": { "status": "published" } }])
        );
    }
}
