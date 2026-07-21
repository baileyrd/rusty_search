use rusty_search_core::{FieldType, Query as CoreQuery, SearchError};
use serde_json::Value;

use crate::schema_map::{FieldMap, FieldMeta};

/// A `Query` translated into what Algolia's `/query` endpoint takes: a
/// free-text `query` string (optionally scoped to one attribute via
/// `restrictSearchableAttributes`), plus a single `filters` expression -
/// Algolia's own filter language, which (like Solr's Lucene syntax, unlike
/// Meilisearch's) supports arbitrary `AND`/`OR`/`NOT` nesting in one
/// string. Unlike Elasticsearch's `must`/`filter` distinction, *every*
/// non-full-text clause is equally non-scoring in Algolia, so `must` and
/// `filter` fold into the same expression here.
///
/// Like `rusty-search-meilisearch`, at most one `Query::Match` clause is
/// supported per query (Algolia has exactly one free-text `query` string
/// per request). Unlike `rusty-search-solr`, `must_not` wrapping a bare
/// `Query::MatchAll`/`Query::Match` is *not* representable, because
/// Algolia's filter language has no "match everything" literal (Lucene's
/// `*:*`) to negate against.
pub struct SearchParams {
    pub query: String,
    pub restrict_searchable_attributes: Option<String>,
    pub filters: Option<String>,
}

pub fn build_search_params(
    query: &CoreQuery,
    fields: &FieldMap,
) -> Result<SearchParams, SearchError> {
    let mut matches = Vec::new();
    let filters = translate(query, fields, &mut matches)?;
    if matches.len() > 1 {
        return Err(SearchError::InvalidQuery(
            "the Algolia backend supports at most one Query::Match clause per query".to_string(),
        ));
    }
    let (restrict_searchable_attributes, query_str) = match matches.into_iter().next() {
        Some((field, value)) => (Some(field), value),
        None => (None, String::new()),
    };
    Ok(SearchParams {
        query: query_str,
        restrict_searchable_attributes,
        filters,
    })
}

/// Translates `query` into a `filters`-expression fragment, recording any
/// `Query::Match` leaves it encounters into `matches` instead of folding
/// them into the expression.
fn translate(
    query: &CoreQuery,
    fields: &FieldMap,
    matches: &mut Vec<(String, String)>,
) -> Result<Option<String>, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(None),

        CoreQuery::Match { field, value } => {
            lookup(fields, field)?;
            matches.push((field.clone(), value.clone()));
            Ok(None)
        }

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            Ok(Some(term_clause(field, meta.field_type, value)?))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            if !matches!(meta.field_type, FieldType::I64 | FieldType::F64) {
                return Err(SearchError::InvalidQuery(format!(
                    "range queries are not supported on {:?} fields in the Algolia backend \
                     (Algolia's filter comparisons require numeric values)",
                    meta.field_type
                )));
            }
            let mut parts = Vec::new();
            if let Some(v) = gte {
                parts.push(format!("{field} >= {}", numeric_literal(v)?));
            }
            if let Some(v) = lte {
                parts.push(format!("{field} <= {}", numeric_literal(v)?));
            }
            match parts.len() {
                0 => Ok(None),
                1 => Ok(Some(parts.into_iter().next().unwrap())),
                _ => Ok(Some(format!("({})", parts.join(" AND ")))),
            }
        }

        CoreQuery::Bool {
            must,
            should,
            must_not,
            filter,
        } => {
            let mut and_parts = Vec::new();
            for q in must.iter().chain(filter.iter()) {
                if let Some(expr) = translate(q, fields, matches)? {
                    and_parts.push(expr);
                }
            }

            let should_children = should
                .iter()
                .map(|q| translate(q, fields, matches))
                .collect::<Result<Vec<_>, _>>()?;
            if !should.is_empty() && !should_children.iter().any(Option::is_none) {
                let or_parts: Vec<String> = should_children.into_iter().flatten().collect();
                if !or_parts.is_empty() {
                    and_parts.push(format!("({})", or_parts.join(" OR ")));
                }
            }

            for q in must_not {
                match translate(q, fields, matches)? {
                    Some(expr) => and_parts.push(format!("NOT ({expr})")),
                    None => {
                        return Err(SearchError::InvalidQuery(
                            "must_not clauses wrapping Query::MatchAll or Query::Match are not \
                             supported by the Algolia backend"
                                .to_string(),
                        ))
                    }
                }
            }

            match and_parts.len() {
                0 => Ok(None),
                1 => Ok(Some(and_parts.into_iter().next().unwrap())),
                _ => Ok(Some(format!("({})", and_parts.join(" AND ")))),
            }
        }
    }
}

fn lookup<'a>(fields: &'a FieldMap, name: &str) -> Result<&'a FieldMeta, SearchError> {
    fields
        .get(name)
        .ok_or_else(|| SearchError::InvalidQuery(format!("unknown field `{name}`")))
}

fn term_clause(field: &str, field_type: FieldType, value: &str) -> Result<String, SearchError> {
    match field_type {
        FieldType::Text | FieldType::Keyword | FieldType::Date => {
            Ok(format!("{field}:{}", quote(value)))
        }
        FieldType::I64 => value
            .parse::<i64>()
            .map(|v| format!("{field} = {v}"))
            .map_err(|e| SearchError::InvalidQuery(format!("expected an integer: {e}"))),
        FieldType::F64 => value
            .parse::<f64>()
            .map(|v| format!("{field} = {v}"))
            .map_err(|e| SearchError::InvalidQuery(format!("expected a float: {e}"))),
        FieldType::Bool => value
            .parse::<bool>()
            .map(|v| format!("{field}:{v}"))
            .map_err(|e| SearchError::InvalidQuery(format!("expected a bool: {e}"))),
    }
}

fn numeric_literal(value: &Value) -> Result<String, SearchError> {
    value
        .as_f64()
        .map(|v| {
            if value.is_i64() || value.is_u64() {
                value.to_string()
            } else {
                v.to_string()
            }
        })
        .ok_or_else(|| SearchError::InvalidQuery("expected a number".to_string()))
}

fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::Query;

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
    fn match_all_has_no_query_or_filters() {
        let params = build_search_params(&Query::match_all(), &fields()).unwrap();
        assert_eq!(params.query, "");
        assert!(params.filters.is_none());
        assert!(params.restrict_searchable_attributes.is_none());
    }

    #[test]
    fn single_match_becomes_query_and_restricts_the_attribute() {
        let params =
            build_search_params(&Query::match_query("title", "rust search"), &fields()).unwrap();
        assert_eq!(params.query, "rust search");
        assert_eq!(
            params.restrict_searchable_attributes.as_deref(),
            Some("title")
        );
        assert!(params.filters.is_none());
    }

    #[test]
    fn two_match_clauses_are_rejected() {
        let q = Query::match_query("title", "rust").or(Query::match_query("title", "async"));
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn term_on_keyword_field_is_quoted() {
        let params = build_search_params(&Query::term("status", "published"), &fields()).unwrap();
        assert_eq!(params.filters.as_deref(), Some("status:\"published\""));
    }

    #[test]
    fn term_on_numeric_field_uses_equals_operator() {
        let params = build_search_params(&Query::term("views", "42"), &fields()).unwrap();
        assert_eq!(params.filters.as_deref(), Some("views = 42"));
    }

    #[test]
    fn range_combines_present_bounds() {
        let params = build_search_params(
            &Query::range("views", Some(10.into()), Some(20.into())),
            &fields(),
        )
        .unwrap();
        assert_eq!(
            params.filters.as_deref(),
            Some("(views >= 10 AND views <= 20)")
        );
    }

    #[test]
    fn range_with_one_bound_has_no_extra_parens() {
        let params =
            build_search_params(&Query::range("views", Some(10.into()), None), &fields()).unwrap();
        assert_eq!(params.filters.as_deref(), Some("views >= 10"));
    }

    #[test]
    fn range_on_keyword_field_errors() {
        let q = Query::range("status", Some("a".into()), None);
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn filter_clauses_fold_into_the_same_filters_expression_as_must() {
        let q =
            Query::term("status", "published").and(Query::range("views", Some(10.into()), None));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filters.as_deref(),
            Some("(status:\"published\" AND views >= 10)")
        );
    }

    #[test]
    fn should_of_terms_becomes_an_or_group() {
        let q = Query::term("status", "published").or(Query::term("status", "archived"));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filters.as_deref(),
            Some("(status:\"published\" OR status:\"archived\")")
        );
    }

    #[test]
    fn not_wraps_term_in_not() {
        let q = Query::term("status", "draft").not();
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.filters.as_deref(), Some("NOT (status:\"draft\")"));
    }

    #[test]
    fn not_wrapping_match_all_is_rejected_unlike_solr() {
        let q = Query::match_all().not();
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn string_literal_escapes_quotes() {
        let params =
            build_search_params(&Query::term("status", "a \"quoted\" value"), &fields()).unwrap();
        assert_eq!(
            params.filters.as_deref(),
            Some("status:\"a \\\"quoted\\\" value\"")
        );
    }
}
