use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An engine-agnostic query expression.
///
/// This is the search equivalent of SQLAlchemy Core's expression language:
/// a small set of composable primitives that every backend knows how to
/// translate into its own native query representation (a Tantivy `Query`,
/// an Elasticsearch query DSL body, a SQL `WHERE` clause, ...).
///
/// Build queries with the associated functions and combine them with
/// [`Query::and`], [`Query::or`] and [`Query::not`]:
///
/// ```
/// use rusty_search_core::Query;
///
/// let q = Query::match_query("body", "async search")
///     .and(Query::term("status", "published"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Query {
    /// Matches every document.
    MatchAll,
    /// Exact match against an untokenized (keyword) field.
    Term { field: String, value: String },
    /// Analyzed full-text match against a text field.
    Match { field: String, value: String },
    /// Inclusive/exclusive numeric or date range filter.
    Range {
        field: String,
        gte: Option<Value>,
        lte: Option<Value>,
    },
    /// Boolean combination of sub-queries, matching Elasticsearch/Lucene
    /// semantics: all `must` and `filter` clauses must match, none of the
    /// `must_not` clauses may match, and - only when `must` is empty - at
    /// least one `should` clause must match. `filter` behaves like `must`
    /// but does not contribute to relevance scoring.
    Bool {
        #[serde(default)]
        must: Vec<Query>,
        #[serde(default)]
        should: Vec<Query>,
        #[serde(default)]
        must_not: Vec<Query>,
        #[serde(default)]
        filter: Vec<Query>,
    },
}

impl Query {
    pub fn match_all() -> Self {
        Query::MatchAll
    }

    pub fn term(field: impl Into<String>, value: impl Into<String>) -> Self {
        Query::Term {
            field: field.into(),
            value: value.into(),
        }
    }

    pub fn match_query(field: impl Into<String>, value: impl Into<String>) -> Self {
        Query::Match {
            field: field.into(),
            value: value.into(),
        }
    }

    pub fn range(field: impl Into<String>, gte: Option<Value>, lte: Option<Value>) -> Self {
        Query::Range {
            field: field.into(),
            gte,
            lte,
        }
    }

    /// Combines two queries such that both must match. Flattens into a
    /// single `Bool` node when either side is already a plain `must`-only
    /// bool query, rather than nesting indefinitely.
    pub fn and(self, other: Query) -> Query {
        match (self, other) {
            (
                Query::Bool {
                    mut must,
                    should,
                    must_not,
                    filter,
                },
                other,
            ) if should.is_empty() && must_not.is_empty() && filter.is_empty() => {
                must.push(other);
                Query::Bool {
                    must,
                    should: vec![],
                    must_not: vec![],
                    filter: vec![],
                }
            }
            (this, other) => Query::Bool {
                must: vec![this, other],
                should: vec![],
                must_not: vec![],
                filter: vec![],
            },
        }
    }

    /// Combines two queries such that at least one must match.
    pub fn or(self, other: Query) -> Query {
        match (self, other) {
            (
                Query::Bool {
                    must,
                    mut should,
                    must_not,
                    filter,
                },
                other,
            ) if must.is_empty() && must_not.is_empty() && filter.is_empty() => {
                should.push(other);
                Query::Bool {
                    must: vec![],
                    should,
                    must_not: vec![],
                    filter: vec![],
                }
            }
            (this, other) => Query::Bool {
                must: vec![],
                should: vec![this, other],
                must_not: vec![],
                filter: vec![],
            },
        }
    }

    /// Negates a query: matches documents that do NOT match `self`.
    // Named to read naturally in the fluent DSL (`query.not()`), not as an
    // operator overload, so it's intentionally not `std::ops::Not`.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Query {
        Query::Bool {
            must: vec![],
            should: vec![],
            must_not: vec![self],
            filter: vec![],
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortOrder {
    Asc,
    Desc,
}

/// A single sort key, applied in order within [`SearchRequest::sort`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Sort {
    /// Sort by relevance score (the default when no sort is specified).
    Score,
    /// Sort by a field's value. The field should have been created with
    /// `fast` enabled in the schema for backends that require it.
    Field { name: String, order: SortOrder },
}

impl Sort {
    pub fn field(name: impl Into<String>, order: SortOrder) -> Self {
        Sort::Field {
            name: name.into(),
            order,
        }
    }
}

/// A complete search request: a query plus pagination and sorting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: Query,
    pub sort: Vec<Sort>,
    pub offset: usize,
    pub limit: usize,
}

impl SearchRequest {
    pub fn new(query: Query) -> Self {
        Self {
            query,
            sort: Vec::new(),
            offset: 0,
            limit: 10,
        }
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn sort(mut self, sort: Sort) -> Self {
        self.sort.push(sort);
        self
    }
}

impl From<Query> for SearchRequest {
    fn from(query: Query) -> Self {
        SearchRequest::new(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn and_flattens_consecutive_must_clauses() {
        let q = Query::term("a", "1")
            .and(Query::term("b", "2"))
            .and(Query::term("c", "3"));

        match q {
            Query::Bool {
                must,
                should,
                must_not,
                filter,
            } => {
                assert_eq!(must.len(), 3);
                assert!(should.is_empty());
                assert!(must_not.is_empty());
                assert!(filter.is_empty());
            }
            other => panic!("expected Bool, got {other:?}"),
        }
    }

    #[test]
    fn or_flattens_consecutive_should_clauses() {
        let q = Query::term("a", "1")
            .or(Query::term("b", "2"))
            .or(Query::term("c", "3"));

        match q {
            Query::Bool { should, .. } => assert_eq!(should.len(), 3),
            other => panic!("expected Bool, got {other:?}"),
        }
    }

    #[test]
    fn not_wraps_in_must_not() {
        let q = Query::term("a", "1").not();
        match q {
            Query::Bool { must_not, .. } => assert_eq!(must_not.len(), 1),
            other => panic!("expected Bool, got {other:?}"),
        }
    }

    #[test]
    fn search_request_builder_defaults() {
        let req: SearchRequest = Query::match_all().into();
        assert_eq!(req.offset, 0);
        assert_eq!(req.limit, 10);
        assert!(req.sort.is_empty());
    }
}
