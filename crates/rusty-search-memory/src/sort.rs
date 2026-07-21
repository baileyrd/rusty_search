use std::cmp::Ordering;

use rusty_search_core::{Document, Sort, SortOrder};
use serde_json::Value;

/// Sorts `scored` in place according to `sorts`, applied in priority order.
/// An empty `sorts` list defaults to descending score, matching every
/// backend's default search order.
pub fn apply(scored: &mut [(f32, &Document)], sorts: &[Sort]) {
    if sorts.is_empty() {
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        return;
    }
    scored.sort_by(|a, b| {
        for sort in sorts {
            let ordering = match sort {
                Sort::Score => b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal),
                Sort::Field { name, order } => {
                    let field_ordering = compare_field(a.1, b.1, name);
                    match order {
                        SortOrder::Asc => field_ordering,
                        SortOrder::Desc => field_ordering.reverse(),
                    }
                }
            };
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    });
}

fn compare_field(a: &Document, b: &Document, name: &str) -> Ordering {
    match (a.get(name), b.get(name)) {
        (Some(a), Some(b)) => compare_values(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, views: i64) -> Document {
        Document::new().with_id(id).set("views", views)
    }

    #[test]
    fn defaults_to_score_descending() {
        let a = doc("a", 1);
        let b = doc("b", 2);
        let mut scored = vec![(0.5, &a), (0.9, &b)];
        apply(&mut scored, &[]);
        assert_eq!(scored[0].1.id.as_deref(), Some("b"));
    }

    #[test]
    fn sorts_by_field_ascending() {
        let a = doc("a", 5);
        let b = doc("b", 1);
        let c = doc("c", 3);
        let mut scored = vec![(1.0, &a), (1.0, &b), (1.0, &c)];
        apply(&mut scored, &[Sort::field("views", SortOrder::Asc)]);
        let ids: Vec<_> = scored
            .iter()
            .map(|(_, d)| d.id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }
}
