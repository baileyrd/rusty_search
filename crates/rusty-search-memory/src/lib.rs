//! An in-memory [`SearchBackend`] implementation for `rusty_search`.
//!
//! [`MemoryBackend`] requires no external search engine and is meant as a
//! reference implementation and a fast, dependency-free backend for tests -
//! the search equivalent of pointing SQLAlchemy at SQLite. It evaluates
//! queries by scanning every document rather than maintaining an inverted
//! index, which keeps the implementation simple and obviously correct at
//! the cost of `O(documents)` search time. Reach for `rusty-search-tantivy`
//! (or another indexed backend) when that stops being fine.

mod eval;
mod sort;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, DocumentId, Hit, Result, Schema, SearchBackend, SearchError, SearchRequest,
    SearchResults,
};

#[derive(Default)]
struct MemoryIndex {
    #[allow(dead_code)] // retained for introspection/future validation use
    schema: Schema,
    documents: HashMap<DocumentId, Document>,
    next_id: u64,
}

/// An in-memory, per-process search backend. Cheaply cloneable - clones
/// share the same underlying indices via an `Arc`.
#[derive(Clone, Default)]
pub struct MemoryBackend {
    indices: Arc<RwLock<HashMap<String, MemoryIndex>>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SearchBackend for MemoryBackend {
    async fn create_index(&self, name: &str, schema: Schema) -> Result<()> {
        let mut indices = self.indices.write().await;
        if indices.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }
        indices.insert(
            name.to_string(),
            MemoryIndex {
                schema,
                documents: HashMap::new(),
                next_id: 0,
            },
        );
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        let mut indices = self.indices.write().await;
        indices
            .remove(name)
            .ok_or_else(|| SearchError::IndexNotFound(name.to_string()))?;
        Ok(())
    }

    async fn index_exists(&self, name: &str) -> Result<bool> {
        Ok(self.indices.read().await.contains_key(name))
    }

    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()> {
        let mut indices = self.indices.write().await;
        let idx = indices
            .get_mut(index)
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))?;
        for mut document in documents {
            let id = document.id.clone().unwrap_or_else(|| {
                idx.next_id += 1;
                format!("_auto_{}", idx.next_id)
            });
            document.id = Some(id.clone());
            idx.documents.insert(id, document);
        }
        Ok(())
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        let mut indices = self.indices.write().await;
        let idx = indices
            .get_mut(index)
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))?;
        idx.documents.remove(id);
        Ok(())
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let indices = self.indices.read().await;
        let idx = indices
            .get(index)
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))?;

        let mut scored: Vec<(f32, &Document)> = idx
            .documents
            .values()
            .filter_map(|doc| eval::matches(&request.query, doc).map(|score| (score, doc)))
            .collect();

        let total = scored.len();
        sort::apply(&mut scored, &request.sort);

        let hits = scored
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .map(|(score, doc)| Hit {
                id: doc.id.clone().unwrap_or_default(),
                score,
                document: doc.clone(),
            })
            .collect();

        Ok(SearchResults { hits, total })
    }

    async fn commit(&self, _index: &str) -> Result<()> {
        // Documents are visible to searches as soon as they're indexed.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{Query, Sort, SortOrder};

    async fn seeded_backend() -> MemoryBackend {
        let backend = MemoryBackend::new();
        backend
            .create_index("articles", Schema::builder().build())
            .await
            .unwrap();
        backend
            .index_batch(
                "articles",
                vec![
                    Document::new()
                        .with_id("1")
                        .set("title", "Rust async search")
                        .set("status", "published")
                        .set("views", 100),
                    Document::new()
                        .with_id("2")
                        .set("title", "Async Rust patterns")
                        .set("status", "draft")
                        .set("views", 10),
                    Document::new()
                        .with_id("3")
                        .set("title", "Cooking with cast iron")
                        .set("status", "published")
                        .set("views", 50),
                ],
            )
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn create_index_rejects_duplicates() {
        let backend = MemoryBackend::new();
        backend
            .create_index("a", Schema::builder().build())
            .await
            .unwrap();
        let err = backend
            .create_index("a", Schema::builder().build())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "a"));
    }

    #[tokio::test]
    async fn operations_on_missing_index_error() {
        let backend = MemoryBackend::new();
        let err = backend
            .search("missing", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "missing"));
    }

    #[tokio::test]
    async fn search_full_text_and_filters() {
        let backend = seeded_backend().await;

        let results = backend
            .search("articles", Query::match_query("title", "async rust").into())
            .await
            .unwrap();
        assert_eq!(results.total, 2);
        assert!(results.hits.iter().all(|h| h.id == "1" || h.id == "2"));

        let results = backend
            .search(
                "articles",
                Query::match_query("title", "async")
                    .and(Query::term("status", "published"))
                    .into(),
            )
            .await
            .unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.hits[0].id, "1");
    }

    #[tokio::test]
    async fn search_respects_sort_and_pagination() {
        let backend = seeded_backend().await;

        let results = backend
            .search(
                "articles",
                SearchRequest::new(Query::term("status", "published"))
                    .sort(Sort::field("views", SortOrder::Asc))
                    .limit(1)
                    .offset(1),
            )
            .await
            .unwrap();
        assert_eq!(results.total, 2);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].id, "1"); // views: 3 -> 50 then 100, offset 1 skips "3"
    }

    #[tokio::test]
    async fn delete_removes_document_from_results() {
        let backend = seeded_backend().await;
        backend.delete("articles", "1").await.unwrap();
        let results = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap();
        assert_eq!(results.total, 2);
        assert!(results.hits.iter().all(|h| h.id != "1"));
    }

    #[tokio::test]
    async fn index_without_id_gets_one_assigned() {
        let backend = MemoryBackend::new();
        backend
            .create_index("a", Schema::builder().build())
            .await
            .unwrap();
        backend
            .index("a", Document::new().set("title", "no id"))
            .await
            .unwrap();
        let results = backend
            .search("a", Query::match_all().into())
            .await
            .unwrap();
        assert_eq!(results.total, 1);
        assert!(!results.hits[0].id.is_empty());
    }

    #[tokio::test]
    async fn delete_index_removes_all_documents() {
        let backend = seeded_backend().await;
        backend.delete_index("articles").await.unwrap();
        assert!(!backend.index_exists("articles").await.unwrap());
        let err = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(_)));
    }
}
