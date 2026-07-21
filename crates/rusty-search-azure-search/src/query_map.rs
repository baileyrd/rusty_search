use rusty_search_core::{FieldType, Query as CoreQuery, SearchError, Sort, SortOrder};
use serde_json::Value;

use crate::schema_map::{FieldMap, FieldMeta};

/// A `Query` translated into what Azure AI Search's `/docs/search` endpoint
/// takes: a single full-Lucene-syntax `search` string (sent with
/// `queryType: "full"`) plus an optional OData `$filter` expression for
/// `Query::Bool::filter` clauses - Azure's own genuinely non-scoring filter
/// mechanism, the same role Solr's `fq` and Elasticsearch's `filter`
/// context play.
///
/// The `search` string uses the same grounding trick `rusty-search-solr`
/// does (ADR-0005): a lone negative clause is invalid Lucene syntax on its
/// own, so a non-trivial tree is wrapped as `* AND (<tree>)`, using Azure's
/// documented `search=*` "match everything" token as the left operand.
/// Because Azure's full Lucene syntax supports arbitrary boolean nesting
/// and field-scoped clauses in one string, exactly like Solr, more than one
/// `Query::Match` is representable here too - unlike
/// `rusty-search-meilisearch`/`rusty-search-algolia`, which cap out at one.
///
/// Two narrower restrictions remain, both disclosed rather than
/// approximated:
/// - `must_not` wrapping a bare `Query::MatchAll` is rejected when it
///   appears among `must`/`should`/`must_not` (i.e. anywhere that
///   translates into the `search` string): unlike Solr's `*:*`, which is a
///   real Lucene clause safe to embed anywhere, Azure's bare `*` is only
///   documented as valid in the single outermost position this module
///   already grounds with - nesting it inside a parenthesized
///   sub-expression isn't a behavior this crate is confident enough in to
///   rely on.
/// - `Query::Match` inside a `Bool::filter` list is rejected: OData's
///   filter grammar has no full-text primitive comparable to Lucene's
///   `field:value` syntax. Use `must`/`should`/`must_not` for full-text
///   clauses instead, which always go through the `search` string.
///
/// The same bare-`Query::MatchAll` negation *is* representable when it
/// appears inside `Bool::filter`, though, since OData has real `true`/
/// `false` boolean literals usable anywhere in its grammar - a more direct
/// analog to Solr's grounding trick than Algolia's filter language allows.
pub struct SearchParams {
    pub search: String,
    pub filter: Option<String>,
}

pub fn build_search_params(
    query: &CoreQuery,
    fields: &FieldMap,
) -> Result<SearchParams, SearchError> {
    let mut filter_parts = Vec::new();
    let expr = translate(query, fields, &mut filter_parts)?;
    let search = match expr {
        Some(e) => format!("* AND ({e})"),
        None => "*".to_string(),
    };
    let filter = match filter_parts.len() {
        0 => None,
        1 => Some(filter_parts.into_iter().next().unwrap()),
        _ => Some(format!("({})", filter_parts.join(" and "))),
    };
    Ok(SearchParams { search, filter })
}

/// Translates `query` into a fragment of the Lucene `search` string,
/// pushing any `Query::Bool::filter` clauses it encounters into
/// `filter_parts` (via [`translate_filter`]) instead of folding them into
/// the returned expression.
fn translate(
    query: &CoreQuery,
    fields: &FieldMap,
    filter_parts: &mut Vec<String>,
) -> Result<Option<String>, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(None),

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            Ok(Some(format!(
                "{field}:{}",
                lucene_literal(meta.field_type, value)?
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
                    "range queries are not supported on {:?} fields in the Azure AI Search backend",
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
                if let Some(expr) = translate_filter(q, fields)? {
                    filter_parts.push(expr);
                }
            }

            let mut and_parts = Vec::new();
            for q in must {
                if let Some(expr) = translate(q, fields, filter_parts)? {
                    and_parts.push(expr);
                }
            }

            let should_children = should
                .iter()
                .map(|q| translate(q, fields, filter_parts))
                .collect::<Result<Vec<_>, _>>()?;
            if !should.is_empty() && !should_children.iter().any(Option::is_none) {
                let or_parts: Vec<String> = should_children.into_iter().flatten().collect();
                if !or_parts.is_empty() {
                    and_parts.push(format!("({})", or_parts.join(" OR ")));
                }
            }

            for q in must_not {
                match translate(q, fields, filter_parts)? {
                    Some(expr) => and_parts.push(format!("NOT ({expr})")),
                    None => {
                        return Err(SearchError::InvalidQuery(
                            "must_not wrapping a bare Query::MatchAll is not supported by the \
                             Azure AI Search backend's `search` translation - move it inside \
                             Query::Bool::filter instead, where OData's true/false literals make \
                             it representable"
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

/// Translates `query` into an OData `$filter` fragment. Only called for
/// `Query::Bool::filter` children - a distinct grammar from [`translate`]'s
/// Lucene `search` string, so it's a separate function rather than a mode
/// flag on the same one.
fn translate_filter(query: &CoreQuery, fields: &FieldMap) -> Result<Option<String>, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(None),

        CoreQuery::Match { .. } => Err(SearchError::InvalidQuery(
            "Query::Match is not supported inside Query::Bool::filter for the Azure AI Search \
             backend - OData's filter grammar has no full-text primitive comparable to Lucene's \
             field:value syntax; use must/should/must_not instead"
                .to_string(),
        )),

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            Ok(Some(format!(
                "{field} eq {}",
                odata_literal(meta.field_type, value)?
            )))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            if !matches!(
                meta.field_type,
                FieldType::I64 | FieldType::F64 | FieldType::Date
            ) {
                return Err(SearchError::InvalidQuery(format!(
                    "range queries are not supported on {:?} fields in the Azure AI Search backend",
                    meta.field_type
                )));
            }
            let mut parts = Vec::new();
            if let Some(v) = gte {
                parts.push(format!("{field} ge {}", range_literal(meta.field_type, v)?));
            }
            if let Some(v) = lte {
                parts.push(format!("{field} le {}", range_literal(meta.field_type, v)?));
            }
            match parts.len() {
                0 => Ok(None),
                1 => Ok(Some(parts.into_iter().next().unwrap())),
                _ => Ok(Some(format!("({})", parts.join(" and ")))),
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
                if let Some(expr) = translate_filter(q, fields)? {
                    and_parts.push(expr);
                }
            }

            let should_children = should
                .iter()
                .map(|q| translate_filter(q, fields))
                .collect::<Result<Vec<_>, _>>()?;
            if !should.is_empty() && !should_children.iter().any(Option::is_none) {
                let or_parts: Vec<String> = should_children.into_iter().flatten().collect();
                if !or_parts.is_empty() {
                    and_parts.push(format!("({})", or_parts.join(" or ")));
                }
            }

            for q in must_not {
                let expr = translate_filter(q, fields)?.unwrap_or_else(|| "true".to_string());
                and_parts.push(format!("not ({expr})"));
            }

            match and_parts.len() {
                0 => Ok(None),
                1 => Ok(Some(and_parts.into_iter().next().unwrap())),
                _ => Ok(Some(format!("({})", and_parts.join(" and ")))),
            }
        }
    }
}

/// Builds Azure's comma-separated `$orderby` clause. Only meaningful when
/// every [`Sort::Field`] entry references a field created with
/// `sortable: true` - callers falling back to in-memory sort don't use
/// this.
pub fn sort_to_orderby(sorts: &[Sort]) -> Option<String> {
    if sorts.is_empty() {
        return None;
    }
    let parts: Vec<String> = sorts
        .iter()
        .map(|sort| match sort {
            Sort::Score => "search.score() desc".to_string(),
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

fn lucene_literal(field_type: FieldType, value: &str) -> Result<String, SearchError> {
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

fn odata_literal(field_type: FieldType, value: &str) -> Result<String, SearchError> {
    match field_type {
        FieldType::Keyword => Ok(odata_quote(value)),
        FieldType::Date => Ok(value.to_string()),
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
        FieldType::Text => unreachable!("Text fields aren't filterable, caller already rejected"),
    }
}

fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn odata_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
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
                    sortable: false,
                },
            ),
            (
                "status".to_string(),
                FieldMeta {
                    field_type: FieldType::Keyword,
                    sortable: true,
                },
            ),
            (
                "views".to_string(),
                FieldMeta {
                    field_type: FieldType::I64,
                    sortable: true,
                },
            ),
            (
                "created_at".to_string(),
                FieldMeta {
                    field_type: FieldType::Date,
                    sortable: true,
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn match_all_is_the_bare_wildcard() {
        let params = build_search_params(&Query::match_all(), &fields()).unwrap();
        assert_eq!(params.search, "*");
        assert!(params.filter.is_none());
    }

    #[test]
    fn term_becomes_a_quoted_field_clause() {
        let params = build_search_params(&Query::term("status", "published"), &fields()).unwrap();
        assert_eq!(params.search, "* AND (status:\"published\")");
    }

    #[test]
    fn term_on_numeric_field_is_unquoted() {
        let params = build_search_params(&Query::term("views", "42"), &fields()).unwrap();
        assert_eq!(params.search, "* AND (views:42)");
    }

    #[test]
    fn match_becomes_a_quoted_phrase_clause() {
        let params =
            build_search_params(&Query::match_query("title", "rust search"), &fields()).unwrap();
        assert_eq!(params.search, "* AND (title:\"rust search\")");
    }

    #[test]
    fn two_match_clauses_are_both_representable_unlike_meilisearch() {
        let q = Query::match_query("title", "rust").or(Query::match_query("title", "async"));
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.search, "* AND ((title:\"rust\" OR title:\"async\"))");
    }

    #[test]
    fn not_wrapping_a_term_is_supported() {
        let q = Query::term("status", "draft").not();
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.search, "* AND (NOT (status:\"draft\"))");
    }

    #[test]
    fn not_wrapping_match_is_supported_unlike_algolia() {
        let q = Query::match_query("title", "async").not();
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.search, "* AND (NOT (title:\"async\"))");
    }

    #[test]
    fn not_wrapping_match_all_in_the_search_string_is_rejected() {
        let q = Query::match_all().not();
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn range_uses_lucene_bracket_syntax() {
        let params = build_search_params(
            &Query::range("views", Some(10.into()), Some(20.into())),
            &fields(),
        )
        .unwrap();
        assert_eq!(params.search, "* AND (views:[10 TO 20])");
    }

    #[test]
    fn range_on_keyword_field_errors() {
        let q = Query::range("status", Some("a".into()), None);
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn filter_clauses_become_a_separate_odata_filter() {
        let q = Query::match_query("title", "rust").and(Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::term("status", "published")],
        });
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("status eq 'published'"));
        assert!(params.search.contains("title:\"rust\""));
        assert!(!params.search.contains("status"));
    }

    #[test]
    fn filter_context_supports_must_not_matchall_unlike_the_search_string() {
        let q = Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::match_all().not()],
        };
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(params.filter.as_deref(), Some("not (true)"));
    }

    #[test]
    fn filter_context_rejects_match() {
        let q = Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::match_query("title", "rust")],
        };
        assert!(build_search_params(&q, &fields()).is_err());
    }

    #[test]
    fn filter_range_combines_both_bounds() {
        let q = Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::range("views", Some(10.into()), Some(20.into()))],
        };
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("(views ge 10 and views le 20)")
        );
    }

    #[test]
    fn filter_date_literal_is_unquoted() {
        let q = Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::term("created_at", "2024-01-01T00:00:00Z")],
        };
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("created_at eq 2024-01-01T00:00:00Z")
        );
    }

    #[test]
    fn odata_string_literal_escapes_single_quotes() {
        let q = Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![],
            filter: vec![Query::term("status", "a 'quoted' value")],
        };
        let params = build_search_params(&q, &fields()).unwrap();
        assert_eq!(
            params.filter.as_deref(),
            Some("status eq 'a ''quoted'' value'")
        );
    }

    #[test]
    fn sort_to_orderby_joins_multiple_keys() {
        let sorts = vec![Sort::field("views", SortOrder::Desc), Sort::Score];
        assert_eq!(
            sort_to_orderby(&sorts).as_deref(),
            Some("views desc, search.score() desc")
        );
    }

    #[test]
    fn sort_to_orderby_is_none_when_empty() {
        assert_eq!(sort_to_orderby(&[]), None);
    }
}
