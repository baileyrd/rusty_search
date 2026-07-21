use std::collections::HashMap;

use rusty_search_core::{Query as CoreQuery, SearchError};
use std::ops::Bound;
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, Query as TantivyQuery, QueryParser, RangeQuery, TermQuery,
};
use tantivy::schema::IndexRecordOption;
use tantivy::Index;

use crate::convert::{json_value_to_term, value_to_term};
use crate::schema_map::FieldMeta;

/// Translates a core [`CoreQuery`] into a boxed Tantivy query, looking up
/// field metadata by name to pick the right `Term`/`Query` construction for
/// each field's type.
pub fn build_query(
    index: &Index,
    fields: &HashMap<String, FieldMeta>,
    query: &CoreQuery,
) -> Result<Box<dyn TantivyQuery>, SearchError> {
    match query {
        CoreQuery::MatchAll => Ok(Box::new(AllQuery)),

        CoreQuery::Term { field, value } => {
            let meta = lookup(fields, field)?;
            let term = value_to_term(meta.field, meta.field_type, value)?;
            Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
        }

        CoreQuery::Match { field, value } => {
            let meta = lookup(fields, field)?;
            let parser = QueryParser::for_index(index, vec![meta.field]);
            parser
                .parse_query(value)
                .map_err(|e| SearchError::InvalidQuery(e.to_string()))
        }

        CoreQuery::Range { field, gte, lte } => {
            let meta = lookup(fields, field)?;
            let lower = match gte {
                Some(v) => Bound::Included(json_value_to_term(meta.field, meta.field_type, v)?),
                None => Bound::Unbounded,
            };
            let upper = match lte {
                Some(v) => Bound::Included(json_value_to_term(meta.field, meta.field_type, v)?),
                None => Bound::Unbounded,
            };
            Ok(Box::new(RangeQuery::new(lower, upper)))
        }

        CoreQuery::Bool {
            must,
            should,
            must_not,
            filter,
        } => {
            let mut clauses: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();
            for q in must.iter().chain(filter.iter()) {
                clauses.push((Occur::Must, build_query(index, fields, q)?));
            }
            for q in should {
                clauses.push((Occur::Should, build_query(index, fields, q)?));
            }
            for q in must_not {
                clauses.push((Occur::MustNot, build_query(index, fields, q)?));
            }
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
    }
}

fn lookup<'a>(
    fields: &'a HashMap<String, FieldMeta>,
    name: &str,
) -> Result<&'a FieldMeta, SearchError> {
    fields
        .get(name)
        .ok_or_else(|| SearchError::InvalidQuery(format!("unknown field `{name}`")))
}
