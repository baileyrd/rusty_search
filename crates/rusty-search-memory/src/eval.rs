use rusty_search_core::{Document, Query};
use serde_json::Value;

/// Evaluates `query` against `doc`, returning its relevance score if it
/// matches or `None` if it doesn't.
///
/// This is a naive, whole-document evaluator rather than an inverted index:
/// it is correct and easy to reason about, which is exactly what you want
/// from a reference/testing backend, but it is `O(documents)` per search.
/// Production-scale workloads should reach for an indexed backend such as
/// `rusty-search-tantivy`.
pub fn matches(query: &Query, doc: &Document) -> Option<f32> {
    match query {
        Query::MatchAll => Some(1.0),
        Query::Term { field, value } => term_matches(doc, field, value).then_some(1.0),
        Query::Match { field, value } => match_score(doc, field, value),
        Query::Range { field, gte, lte } => {
            range_matches(doc, field, gte.as_ref(), lte.as_ref()).then_some(1.0)
        }
        Query::Bool {
            must,
            should,
            must_not,
            filter,
        } => bool_matches(doc, must, should, must_not, filter),
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn term_matches(doc: &Document, field: &str, value: &str) -> bool {
    match doc.get(field) {
        Some(Value::Array(items)) => items
            .iter()
            .any(|item| value_to_string(item).as_deref() == Some(value)),
        Some(other) => value_to_string(other).as_deref() == Some(value),
        None => false,
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn match_score(doc: &Document, field: &str, value: &str) -> Option<f32> {
    let text = match doc.get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => value_to_string(other)?,
        None => return None,
    };
    let doc_tokens = tokenize(&text);
    let query_tokens = tokenize(value);
    if query_tokens.is_empty() {
        return None;
    }
    let matched = query_tokens
        .iter()
        .filter(|token| doc_tokens.contains(token))
        .count();
    if matched == 0 {
        None
    } else {
        Some(matched as f32 / query_tokens.len() as f32)
    }
}

fn range_matches(doc: &Document, field: &str, gte: Option<&Value>, lte: Option<&Value>) -> bool {
    let Some(value) = doc.get(field) else {
        return false;
    };
    let Some(n) = value.as_f64() else {
        return false;
    };
    if let Some(gte) = gte.and_then(Value::as_f64) {
        if n < gte {
            return false;
        }
    }
    if let Some(lte) = lte.and_then(Value::as_f64) {
        if n > lte {
            return false;
        }
    }
    true
}

fn bool_matches(
    doc: &Document,
    must: &[Query],
    should: &[Query],
    must_not: &[Query],
    filter: &[Query],
) -> Option<f32> {
    if must_not.iter().any(|q| matches(q, doc).is_some()) {
        return None;
    }
    for q in filter {
        matches(q, doc)?;
    }

    let mut score = 0.0f32;
    for q in must {
        score += matches(q, doc)?;
    }

    let mut should_score = 0.0f32;
    let mut should_matched = 0usize;
    for q in should {
        if let Some(s) = matches(q, doc) {
            should_score += s;
            should_matched += 1;
        }
    }

    if must.is_empty() {
        if !should.is_empty() && should_matched == 0 {
            // With no `must` clauses, at least one `should` must match -
            // matching Elasticsearch/Lucene bool query semantics.
            return None;
        }
        // Baseline score for a query with no `must` clause (pure
        // filter/must_not/should), consistent with `MatchAll`'s score.
        score += 1.0;
    }
    score += should_score;

    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document::new()
            .with_id("1")
            .set("title", "Rust async search")
            .set("status", "published")
            .set("views", 42)
    }

    #[test]
    fn match_all_always_matches() {
        assert_eq!(matches(&Query::match_all(), &doc()), Some(1.0));
    }

    #[test]
    fn term_requires_exact_match() {
        assert!(matches(&Query::term("status", "published"), &doc()).is_some());
        assert!(matches(&Query::term("status", "Published"), &doc()).is_none());
    }

    #[test]
    fn match_scores_by_token_overlap() {
        let score = matches(&Query::match_query("title", "rust search"), &doc()).unwrap();
        assert_eq!(score, 1.0);
        let partial = matches(&Query::match_query("title", "rust golang"), &doc()).unwrap();
        assert_eq!(partial, 0.5);
        assert!(matches(&Query::match_query("title", "golang"), &doc()).is_none());
    }

    #[test]
    fn range_filters_numerically() {
        assert!(matches(
            &Query::range("views", Some(40.into()), Some(50.into())),
            &doc()
        )
        .is_some());
        assert!(matches(&Query::range("views", Some(43.into()), None), &doc()).is_none());
    }

    #[test]
    fn bool_must_not_excludes() {
        let q = Query::match_all().and(Query::term("status", "published").not());
        assert!(matches(&q, &doc()).is_none());
    }

    #[test]
    fn bool_should_requires_one_match_when_must_empty() {
        let q = Query::term("status", "draft").or(Query::term("status", "archived"));
        assert!(matches(&q, &doc()).is_none());

        let q = Query::term("status", "published").or(Query::term("status", "archived"));
        assert!(matches(&q, &doc()).is_some());
    }
}
