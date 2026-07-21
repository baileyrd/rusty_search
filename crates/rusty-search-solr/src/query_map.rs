use rusty_search_core::{FieldType, Query as CoreQuery, SearchError, Sort, SortOrder};
use serde_json::Value;

use crate::schema_map::{FieldMap, FieldMeta};

/// A `Query` translated into what Solr's classic `/select` handler takes:
/// a single Lucene query string (`q`) plus a list of separate filter
/// queries (`fq`) - Solr's own genuinely non-scoring filter mechanism,
/// analogous to Elasticsearch's `bool` query `filter` context (and unlike
/// `rusty-search-tantivy`, which has to approximate `filter` as `must`).
///
/// Unlike `rusty-search-meilisearch`, this backend can represent an entire
/// `Query` tree - including more than one `Query::Match` and `must_not`
/// wrapping a bare `Query::MatchAll`/`Query::Match` - because Lucene's
/// query syntax supports arbitrary boolean nesting in one string. The
/// final `q` is always grounded as `*:* AND (<tree>)` (or just `*:*` for a
/// trivial tree) so a lone negative clause like `Query::match_all().not()`
/// still parses as a well-formed query instead of an invalid bare `NOT`.
pub struct SearchParams {
    pub q: String,
    pub fq: Vec<String>,
}

pub fn build_search_params(
    query: &CoreQuery,
    fields: &FieldMap,
) -> Result<SearchParams, SearchError> {
    let mut fq = Vec::new();
    let expr = translate(query, fields, &mut fq)?;
    let q = match expr {
        Some(e) => format!("*:* AND ({e})"),
        None => "*:*".to_string(),
    };
    Ok(SearchParams { q, fq })
}

/// Translates `query` into a `q`-expression fragment, pushing any
/// `Query::Bool::filter` clauses it encounters directly into `fq` instead
/// of folding them into the returned expression.
fn translate(
    query: &CoreQuery,
    fields: &FieldMap,
    fq: &mut Vec<String>,
) -> Result<Option<String>, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(None),

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            Ok(Some(format!(
                "{field}:{}",
                literal(meta.field_type, value)?
            )))
        }

        CoreQuery::Match { field, value } => {
            lookup(fields, field)?;
            Ok(Some(format!("{field}:{}", quote(value))))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            if !matches!(
                meta.field_type,
                FieldType::I64 | FieldType::F64 | FieldType::Date
            ) {
                return Err(SearchError::InvalidQuery(format!(
                    "range queries are not supported on {:?} fields in the Solr backend",
                    meta.field_type
                )));
            }
            let lower = match gte {
                Some(v) => range_literal(meta.field_type, v)?,
                None => "*".to_string(),
            };
            let upper = match lte {
                Some(v) => range_literal(meta.field_type, v)?,
                None => "*".to_string(),
            };
            Ok(Some(format!("{field}:[{lower} TO {upper}]")))
        }

        CoreQuery::Bool {
            must,
            should,
            must_not,
            filter,
        } => {
            for q in filter {
                if let Some(expr) = translate(q, fields, fq)? {
                    fq.push(expr);
                }
            }

            let mut and_parts = Vec::new();
            for q in must {
                if let Some(expr) = translate(q, fields, fq)? {
                    and_parts.push(expr);
                }
            }

            let should_children = should
                .iter()
                .map(|q| translate(q, fields, fq))
                .collect::<Result<Vec<_>, _>>()?;
            // A `should` arm that's `None` (a `MatchAll` leaf) is trivially
            // satisfiable, making the whole OR group trivially true - so it
            // drops out of the expression entirely.
            if !should.is_empty() && !should_children.iter().any(Option::is_none) {
                let or_parts: Vec<String> = should_children.into_iter().flatten().collect();
                if !or_parts.is_empty() {
                    and_parts.push(format!("({})", or_parts.join(" OR ")));
                }
            }

            for q in must_not {
                let expr = translate(q, fields, fq)?.unwrap_or_else(|| "*:*".to_string());
                and_parts.push(format!("NOT ({expr})"));
            }

            match and_parts.len() {
                0 => Ok(None),
                1 => Ok(Some(and_parts.into_iter().next().unwrap())),
                _ => Ok(Some(format!("({})", and_parts.join(" AND ")))),
            }
        }
    }
}

/// Builds Solr's comma-separated `sort` parameter, or `None` for the
/// default (relevance) order.
pub fn sort_to_solr(sorts: &[Sort]) -> Option<String> {
    if sorts.is_empty() {
        return None;
    }
    let parts: Vec<String> = sorts
        .iter()
        .map(|sort| match sort {
            Sort::Score => "score desc".to_string(),
            Sort::Field { name, order } => {
                let order_str = match order {
                    SortOrder::Asc => "asc",
                    SortOrder::Desc => "desc",
                };
                format!("{name} {order_str}")
            }
        })
        .collect();
    Some(parts.join(", "))
}

fn lookup<'a>(fields: &'a FieldMap, name: &str) -> Result<&'a FieldMeta, SearchError> {
    fields
        .get(name)
        .ok_or_else(|| SearchError::InvalidQuery(format!("unknown field `{name}`")))
}

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

fn range_literal(field_type: FieldType, value: &Value) -> Result<String, SearchError> {
    match field_type {
        FieldType::I64 => value
            .as_i64()
            .map(|v| v.to_string())
            .ok_or_else(|| SearchError::InvalidQuery("expected an integer".to_string())),
        FieldType::F64 => value
            .as_f64()
            .map(|v| v.to_string())
            .ok_or_else(|| SearchError::InvalidQuery("expected a number".to_string())),
        FieldType::Date => value.as_str().map(|s| s.to_string()).ok_or_else(|| {
            SearchError::InvalidQuery("expected an RFC 3339 date string".to_string())
        }),
        other => unreachable!("caller already validated field_type is I64/F64/Date, got {other:?}"),
    }
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
            (
                "created_at".to_string(),
                FieldMeta {
                    field_type: FieldType::Date,
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn match_all_is_the_bare_wildcard() {
        let params = build_search_params(&Query::match_all(), &fields()).unwrap();
        assert_eq!(params.q, "*:*");
        assert!(params.fq.is_empty());
    }

    #[test]
    fn term_becomes_a_quoted_field_clause() {
        let params = build_search_params(&Query::term("status", "published"), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (status:\"published\")");
    }

    #[test]
    fn term_on_numeric_field_is_unquoted() {
        let params = build_search_params(&Query::term("views", "42"), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (views:42)");
    }

    #[test]
    fn match_becomes_a_quoted_phrase_clause() {
        let params =
            build_search_params(&Query::match_query("title", "rust search"), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (title:\"rust search\")");
    }

    #[test]
    fn range_uses_lucene_bracket_syntax() {
        let params = build_search_params(
            &Query::range("views", Some(10.into()), Some(20.into())),
            &fields(),
        )
        .unwrap();
        assert_eq!(params.q, "*:* AND (views:[10 TO 20])");
    }

    #[test]
    fn range_with_one_bound_uses_a_wildcard_for_the_other() {
        let params =
            build_search_params(&Query::range("views", Some(10.into()), None), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (views:[10 TO *])");
    }

    #[test]
    fn range_supports_date_fields_unlike_meilisearch() {
        let q = Query::range("created_at", Some("2024-01-01T00:00:00Z".into()), None);
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (created_at:[2024-01-01T00:00:00Z TO *])");
    }

    #[test]
    fn range_on_keyword_field_errors() {
        let q = Query::range("status", Some("a".into()), None);
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn filter_clauses_become_separate_fq_entries() {
        let q = Query::match_query("title", "rust").and(Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::term("status", "published")],
        });
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.fq, vec!["status:\"published\"".to_string()]);
        assert!(params.q.contains("title:\"rust\""));
        assert!(!params.q.contains("status"));
    }

    #[test]
    fn should_of_terms_becomes_an_or_group() {
        let q = Query::term("status", "published").or(Query::term("status", "archived"));
        let params = build_search_params(&q, &fields()).unwrap();
        // The inner OR group is already parenthesized by the Bool
        // translation, and the top-level grounding wrap adds its own -
        // redundant but harmless, valid Lucene syntax either way.
        assert_eq!(
            params.q,
            "*:* AND ((status:\"published\" OR status:\"archived\"))"
        );
    }

    #[test]
    fn not_wrapping_match_all_is_representable_unlike_meilisearch() {
        let params = build_search_params(&Query::match_all().not(), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (NOT (*:*))");
    }

    #[test]
    fn two_match_clauses_are_both_representable_unlike_meilisearch() {
        let q = Query::match_query("title", "rust").or(Query::match_query("title", "async"));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.q, "*:* AND ((title:\"rust\" OR title:\"async\"))");
    }

    #[test]
    fn string_literal_escapes_quotes() {
        let params =
            build_search_params(&Query::term("status", "a \"quoted\" value"), &fields()).unwrap();
        assert_eq!(params.q, "*:* AND (status:\"a \\\"quoted\\\" value\")");
    }

    #[test]
    fn sort_to_solr_joins_multiple_keys() {
        let sorts = vec![Sort::field("views", SortOrder::Desc), Sort::Score];
        assert_eq!(
            sort_to_solr(&sorts).as_deref(),
            Some("views desc, score desc")
        );
    }

    #[test]
    fn sort_to_solr_is_none_when_empty() {
        assert_eq!(sort_to_solr(&[]), None);
    }
}
