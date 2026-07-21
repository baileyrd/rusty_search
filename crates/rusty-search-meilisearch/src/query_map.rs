use rusty_search_core::{FieldType, Query as CoreQuery, SearchError};
use serde_json::Value;

use crate::schema_map::{FieldMap, FieldMeta};

/// A `Query` translated into what a Meilisearch search request actually
/// takes: an optional filter expression, and at most one full-text
/// `(field, value)` pair (Meilisearch's `q` + `attributesToSearchOn`).
///
/// Meilisearch's search API has exactly one free-text query string per
/// request - there's no way to combine several independent full-text
/// clauses the way `Query::Bool` can nest several `Query::Match`es. So
/// `Query::Match` clauses are pulled out of the tree separately from
/// everything else (`Query::Term`/`Query::Range`/`Query::Bool`'s boolean
/// structure), which becomes a Meilisearch filter expression string. See
/// the crate's module docs for exactly which trees are - and aren't -
/// representable this way.
pub struct SearchParams {
    pub filter: Option<String>,
    pub full_text: Option<(String, String)>,
}

pub fn build_search_params(
    query: &CoreQuery,
    fields: &FieldMap,
) -> Result<SearchParams, SearchError> {
    let mut matches = Vec::new();
    let filter = split(query, fields, &mut matches)?;
    if matches.len() > 1 {
        return Err(SearchError::InvalidQuery(
            "the Meilisearch backend supports at most one Query::Match clause per query"
                .to_string(),
        ));
    }
    Ok(SearchParams {
        filter,
        full_text: matches.into_iter().next(),
    })
}

/// Translates `query`'s non-full-text structure into a filter expression
/// fragment, recording any `Query::Match` leaves it encounters into
/// `matches` instead of trying to fold them into the filter string.
fn split(
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
            Ok(Some(format!(
                "{field} = {}",
                literal(meta.field_type, value)?
            )))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            if !matches!(meta.field_type, FieldType::I64 | FieldType::F64) {
                return Err(SearchError::InvalidQuery(format!(
                    "range queries are not supported on {:?} fields in the Meilisearch backend \
                     (only I64/F64 fields support ordering comparisons in Meilisearch filters)",
                    meta.field_type
                )));
            }
            let mut bounds = Vec::new();
            if let Some(v) = gte {
                bounds.push(format!("{field} >= {}", numeric_literal(v)?));
            }
            if let Some(v) = lte {
                bounds.push(format!("{field} <= {}", numeric_literal(v)?));
            }
            if bounds.is_empty() {
                Ok(None)
            } else {
                Ok(Some(format!("({})", bounds.join(" AND "))))
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
                if let Some(expr) = split(q, fields, matches)? {
                    and_parts.push(expr);
                }
            }

            let should_children = should
                .iter()
                .map(|q| split(q, fields, matches))
                .collect::<Result<Vec<_>, _>>()?;
            // A `should` arm that's `None` (a `MatchAll`/`Match` leaf) is
            // trivially satisfiable, which makes the entire OR group
            // trivially true - so it drops out of the filter entirely,
            // same as an `Option<bool>` OR-ed with a bare `true`.
            if !should.is_empty() && !should_children.iter().any(Option::is_none) {
                let or_parts: Vec<String> = should_children.into_iter().flatten().collect();
                if !or_parts.is_empty() {
                    and_parts.push(format!("({})", or_parts.join(" OR ")));
                }
            }

            for q in must_not {
                match split(q, fields, matches)? {
                    Some(expr) => and_parts.push(format!("NOT ({expr})")),
                    None => {
                        return Err(SearchError::InvalidQuery(
                            "must_not clauses wrapping Query::MatchAll or Query::Match are not \
                             supported by the Meilisearch backend"
                                .to_string(),
                        ))
                    }
                }
            }

            match and_parts.len() {
                0 => Ok(None),
                // Avoid redundant outer parens when there's nothing to
                // combine with AND.
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

/// Renders a `Query::Term`'s string value as a Meilisearch filter literal
/// for `field_type`: a quoted string for text-ish/date fields, a bare
/// number/boolean otherwise.
fn literal(field_type: FieldType, value: &str) -> Result<String, SearchError> {
    match field_type {
        FieldType::Text | FieldType::Keyword | FieldType::Date => Ok(quote(value)),
        FieldType::I64 => value
            .parse::<i64>()
            .map(|v| v.to_string())
            .map_err(|e| SearchError::InvalidQuery(format!("expected an integer: {e}"))),
        FieldType::F64 => value
            .parse::<f64>()
            .map(|v| v.to_string())
            .map_err(|e| SearchError::InvalidQuery(format!("expected a float: {e}"))),
        FieldType::Bool => value
            .parse::<bool>()
            .map(|v| v.to_string())
            .map_err(|e| SearchError::InvalidQuery(format!("expected a bool: {e}"))),
    }
}

fn numeric_literal(value: &Value) -> Result<String, SearchError> {
    value
        .as_f64()
        .map(|v| {
            if value.is_i64() || value.is_u64() {
                // preserve integer formatting rather than "10.0"
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
    fn match_all_has_no_filter_or_full_text() {
        let params = build_search_params(&Query::match_all(), &fields()).unwrap();
        assert!(params.filter.is_none());
        assert!(params.full_text.is_none());
    }

    #[test]
    fn single_match_becomes_full_text_not_a_filter() {
        let params =
            build_search_params(&Query::match_query("title", "rust search"), &fields()).unwrap();
        assert!(params.filter.is_none());
        assert_eq!(
            params.full_text,
            Some(("title".to_string(), "rust search".to_string()))
        );
    }

    #[test]
    fn term_becomes_a_quoted_filter_literal() {
        let params = build_search_params(&Query::term("status", "published"), &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("status = \"published\""));
    }

    #[test]
    fn term_on_numeric_field_is_unquoted() {
        let params = build_search_params(&Query::term("views", "42"), &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("views = 42"));
    }

    #[test]
    fn range_combines_present_bounds() {
        let params = build_search_params(
            &Query::range("views", Some(10.into()), Some(20.into())),
            &fields(),
        )
        .unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("(views >= 10 AND views <= 20)")
        );
    }

    #[test]
    fn range_on_keyword_field_errors() {
        let q = Query::range("status", Some("a".into()), None);
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn match_and_term_combine_filter_plus_full_text() {
        let q = Query::match_query("title", "rust").and(Query::term("status", "published"));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("status = \"published\""));
        assert_eq!(
            params.full_text,
            Some(("title".to_string(), "rust".to_string()))
        );
    }

    #[test]
    fn two_match_clauses_are_rejected() {
        let q = Query::match_query("title", "rust").or(Query::match_query("title", "async"));
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn should_of_terms_becomes_or_group() {
        let q = Query::term("status", "published").or(Query::term("status", "archived"));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("(status = \"published\" OR status = \"archived\")")
        );
    }

    #[test]
    fn not_wraps_term_in_not() {
        let q = Query::term("status", "draft").not();
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("NOT (status = \"draft\")"));
    }

    #[test]
    fn not_wrapping_match_all_is_rejected() {
        let q = Query::match_all().not();
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn string_literal_escapes_quotes() {
        let params =
            build_search_params(&Query::term("status", "a \"quoted\" value"), &fields()).unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("status = \"a \\\"quoted\\\" value\"")
        );
    }
}
