//! A [`SearchBackend`] implementation backed by [Tantivy](https://github.com/quickwit-oss/tantivy),
//! an embedded full-text search engine (no server to run, unlike
//! Elasticsearch/Meilisearch/OpenSearch).
//!
//! Use [`TantivyBackend::in_memory`] for an ephemeral, process-local index
//! (great for tests, or workloads that can rebuild their index on startup),
//! or [`TantivyBackend::on_disk`] to persist segments to a directory.
//!
//! ## Known limitations
//!
//! - Native, index-accelerated sorting is only wired up for a single
//!   [`Sort::Field`] on an `i64`/`f64` field created with `fast: true`.
//!   Sorting by a `Keyword`/`Text`/`Bool`/`Date` field, or by more than one
//!   key, falls back to fetching a bounded set of top-scoring matches (see
//!   [`FALLBACK_SORT_CAP`]) and sorting them in memory - correct up to that
//!   cap, not beyond it.
//! - `Query::Bool`'s `filter` clauses are folded into `must` (Tantivy's
//!   `BooleanQuery` has no non-scoring "filter" occur), so they still
//!   participate in scoring rather than being score-neutral.
//! - `TantivyBackend::on_disk` does not reopen indices that already exist
//!   on disk from a previous process - `create_index` always creates fresh
//!   segments and errors if the directory already holds one.

mod convert;
mod query_map;
mod schema_map;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, FieldType as CoreFieldType, Hit, Result, Schema as CoreSchema, SearchBackend,
    SearchError, SearchRequest, SearchResults, Sort, SortOrder,
};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::Query as TantivyQuery;
use tantivy::schema::document::TantivyDocument;
use tantivy::schema::Schema as TantivySchema;
use tantivy::{Index, IndexReader, IndexWriter, Order, Searcher, Term};

use convert::{document_to_tantivy, tantivy_doc_to_document};
use schema_map::{build_tantivy_schema, FieldMeta};

/// Search results beyond a single [`Sort::Field`] on a fast numeric field
/// fall back to sorting an in-memory candidate set capped at this many
/// top-scoring documents.
pub const FALLBACK_SORT_CAP: usize = 10_000;

struct IndexHandle {
    index: Index,
    tantivy_schema: TantivySchema,
    fields: HashMap<String, FieldMeta>,
    id_field: tantivy::schema::Field,
    writer: StdMutex<IndexWriter<TantivyDocument>>,
    reader: IndexReader,
}

/// A Tantivy-backed [`SearchBackend`]. Cheaply cloneable - clones share the
/// same underlying indices via an `Arc`.
#[derive(Clone)]
pub struct TantivyBackend {
    data_dir: Option<PathBuf>,
    indices: Arc<RwLock<HashMap<String, Arc<IndexHandle>>>>,
}

impl TantivyBackend {
    /// Creates a backend whose indices live entirely in memory and vanish
    /// when the backend is dropped.
    pub fn in_memory() -> Self {
        Self {
            data_dir: None,
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Creates a backend that persists each index's segments under
    /// `dir/<index name>/`.
    pub fn on_disk(dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: Some(dir.into()),
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for TantivyBackend {
    fn default() -> Self {
        Self::in_memory()
    }
}

fn backend_err(e: impl std::error::Error + Send + Sync + 'static) -> SearchError {
    SearchError::Backend(anyhow::Error::new(e))
}

#[async_trait]
impl SearchBackend for TantivyBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        let mut indices = self.indices.write().await;
        if indices.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let mapped = build_tantivy_schema(&schema);
        let index = match &self.data_dir {
            None => Index::create_in_ram(mapped.tantivy_schema.clone()),
            Some(dir) => {
                let index_dir = dir.join(name);
                if index_dir.join("meta.json").exists() {
                    return Err(SearchError::IndexAlreadyExists(name.to_string()));
                }
                std::fs::create_dir_all(&index_dir).map_err(backend_err)?;
                Index::create_in_dir(&index_dir, mapped.tantivy_schema.clone())
                    .map_err(backend_err)?
            }
        };

        let writer: IndexWriter<TantivyDocument> = index.writer(50_000_000).map_err(backend_err)?;
        let reader = index.reader().map_err(backend_err)?;

        indices.insert(
            name.to_string(),
            Arc::new(IndexHandle {
                index,
                tantivy_schema: mapped.tantivy_schema,
                fields: mapped.fields,
                id_field: mapped.id_field,
                writer: StdMutex::new(writer),
                reader,
            }),
        );
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        let mut indices = self.indices.write().await;
        indices
            .remove(name)
            .ok_or_else(|| SearchError::IndexNotFound(name.to_string()))?;
        if let Some(dir) = &self.data_dir {
            let _ = std::fs::remove_dir_all(dir.join(name));
        }
        Ok(())
    }

    async fn index_exists(&self, name: &str) -> Result<bool> {
        Ok(self.indices.read().await.contains_key(name))
    }

    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()> {
        let handle = self.handle(index).await?;
        let writer = handle.writer.lock().expect("writer mutex poisoned");
        for document in documents {
            let (id, tantivy_doc) = document_to_tantivy(&handle.tantivy_schema, document);
            // Insert-or-replace: clear out any previous document with this id first.
            writer.delete_term(Term::from_field_text(handle.id_field, &id));
            writer.add_document(tantivy_doc).map_err(backend_err)?;
        }
        Ok(())
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        let handle = self.handle(index).await?;
        let writer = handle.writer.lock().expect("writer mutex poisoned");
        writer.delete_term(Term::from_field_text(handle.id_field, id));
        Ok(())
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let handle = self.handle(index).await?;

        let tantivy_query = query_map::build_query(&handle.index, &handle.fields, &request.query)?;
        let searcher = handle.reader.searcher();

        let total = searcher
            .search(tantivy_query.as_ref(), &Count)
            .map_err(backend_err)?;

        let scored = run_search(
            &searcher,
            tantivy_query.as_ref(),
            &handle.tantivy_schema,
            &handle.fields,
            &request,
        )?;

        let hits = scored
            .into_iter()
            .map(|(score, document)| {
                let id = document.id.clone().unwrap_or_default();
                Hit {
                    id,
                    score,
                    document,
                }
            })
            .collect();

        Ok(SearchResults { hits, total })
    }

    async fn commit(&self, index: &str) -> Result<()> {
        let handle = self.handle(index).await?;
        {
            let mut writer = handle.writer.lock().expect("writer mutex poisoned");
            writer.commit().map_err(backend_err)?;
        }
        // Force the reader to pick up the commit synchronously rather than
        // relying on `ReloadPolicy::OnCommitWithDelay`'s background timer,
        // so `commit()` reliably makes the change visible to callers.
        handle.reader.reload().map_err(backend_err)?;
        Ok(())
    }
}

impl TantivyBackend {
    async fn handle(&self, index: &str) -> Result<Arc<IndexHandle>> {
        self.indices
            .read()
            .await
            .get(index)
            .cloned()
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))
    }
}

/// Picks a search strategy for `request` and returns matching `(score,
/// Document)` pairs already sorted, offset, and limited.
fn run_search(
    searcher: &Searcher,
    query: &dyn TantivyQuery,
    schema: &TantivySchema,
    fields: &HashMap<String, FieldMeta>,
    request: &SearchRequest,
) -> Result<Vec<(f32, Document)>> {
    if let [Sort::Field { name, order }] = request.sort.as_slice() {
        if let Some(meta) = fields.get(name) {
            if meta.fast && matches!(meta.field_type, CoreFieldType::I64 | CoreFieldType::F64) {
                return native_fast_field_sort(
                    searcher, query, schema, fields, *meta, *order, request,
                );
            }
        }
    }
    fallback_sort(searcher, query, schema, fields, request)
}

fn native_fast_field_sort(
    searcher: &Searcher,
    query: &dyn TantivyQuery,
    schema: &TantivySchema,
    fields: &HashMap<String, FieldMeta>,
    meta: FieldMeta,
    order: SortOrder,
    request: &SearchRequest,
) -> Result<Vec<(f32, Document)>> {
    let tantivy_order = match order {
        SortOrder::Asc => Order::Asc,
        SortOrder::Desc => Order::Desc,
    };
    let collector = TopDocs::with_limit(request.limit).and_offset(request.offset);
    let field_name = fields
        .iter()
        .find(|(_, m)| m.field == meta.field)
        .map(|(name, _)| name.clone())
        .expect("field metadata came from this fields map");

    let resolved = match meta.field_type {
        CoreFieldType::I64 => searcher
            .search(
                query,
                &collector.order_by_fast_field::<i64>(field_name, tantivy_order),
            )
            .map_err(backend_err)?
            .into_iter()
            .map(|(v, addr)| (v.unwrap_or_default() as f32, addr))
            .collect::<Vec<_>>(),
        CoreFieldType::F64 => searcher
            .search(
                query,
                &collector.order_by_fast_field::<f64>(field_name, tantivy_order),
            )
            .map_err(backend_err)?
            .into_iter()
            .map(|(v, addr)| (v.unwrap_or_default() as f32, addr))
            .collect::<Vec<_>>(),
        _ => unreachable!("caller only routes I64/F64 fast fields here"),
    };

    resolved
        .into_iter()
        .map(|(score, addr)| {
            let tantivy_doc: TantivyDocument = searcher.doc(addr).map_err(backend_err)?;
            Ok((score, tantivy_doc_to_document(&tantivy_doc, schema, fields)))
        })
        .collect()
}

fn fallback_sort(
    searcher: &Searcher,
    query: &dyn TantivyQuery,
    schema: &TantivySchema,
    fields: &HashMap<String, FieldMeta>,
    request: &SearchRequest,
) -> Result<Vec<(f32, Document)>> {
    let cap = FALLBACK_SORT_CAP.max(request.offset + request.limit);
    let top_docs = TopDocs::with_limit(cap).order_by_score();
    let ranked = searcher.search(query, &top_docs).map_err(backend_err)?;

    let mut scored: Vec<(f32, Document)> = ranked
        .into_iter()
        .map(|(score, addr)| {
            let tantivy_doc: TantivyDocument = searcher
                .doc(addr)
                .expect("doc address from this searcher must resolve");
            (score, tantivy_doc_to_document(&tantivy_doc, schema, fields))
        })
        .collect();

    sort_in_memory(&mut scored, &request.sort);

    Ok(scored
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect())
}

fn sort_in_memory(scored: &mut [(f32, Document)], sorts: &[Sort]) {
    use std::cmp::Ordering;
    if sorts.is_empty() {
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        return;
    }
    scored.sort_by(|a, b| {
        for sort in sorts {
            let ordering = match sort {
                Sort::Score => b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal),
                Sort::Field { name, order } => {
                    let field_ordering = compare_field(&a.1, &b.1, name);
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

fn compare_field(a: &Document, b: &Document, name: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.get(name), b.get(name)) {
        (Some(a), Some(b)) => compare_values(a, b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_values(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    use serde_json::Value;
    use std::cmp::Ordering;
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
    use rusty_search_core::{FieldOptions, Query, Schema};

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    async fn seeded_backend() -> TantivyBackend {
        let backend = TantivyBackend::in_memory();
        backend
            .create_index("articles", articles_schema())
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
        backend.commit("articles").await.unwrap();
        backend
    }

    #[tokio::test]
    async fn create_index_rejects_duplicates() {
        let backend = TantivyBackend::in_memory();
        backend.create_index("a", articles_schema()).await.unwrap();
        let err = backend
            .create_index("a", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "a"));
    }

    #[tokio::test]
    async fn operations_on_missing_index_error() {
        let backend = TantivyBackend::in_memory();
        let err = backend
            .search("missing", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "missing"));
    }

    #[tokio::test]
    async fn full_text_match_finds_relevant_documents() {
        let backend = seeded_backend().await;
        let results = backend
            .search("articles", Query::match_query("title", "async rust").into())
            .await
            .unwrap();
        assert_eq!(results.total, 2);
        let ids: std::collections::HashSet<_> = results.hits.iter().map(|h| h.id.clone()).collect();
        assert!(ids.contains("1"));
        assert!(ids.contains("2"));
    }

    #[tokio::test]
    async fn term_and_bool_queries_filter_exactly() {
        let backend = seeded_backend().await;
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
    async fn range_query_filters_numerically() {
        let backend = seeded_backend().await;
        let results = backend
            .search(
                "articles",
                Query::range("views", Some(60.into()), None).into(),
            )
            .await
            .unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.hits[0].id, "1");
    }

    #[tokio::test]
    async fn native_fast_field_sort_orders_by_views() {
        let backend = seeded_backend().await;
        let results = backend
            .search(
                "articles",
                SearchRequest::new(Query::term("status", "published"))
                    .sort(Sort::field("views", SortOrder::Asc)),
            )
            .await
            .unwrap();
        let ids: Vec<_> = results.hits.iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids, vec!["3", "1"]); // views: 50 then 100
    }

    #[tokio::test]
    async fn fallback_sort_orders_by_non_fast_keyword_field() {
        let backend = seeded_backend().await;
        let results = backend
            .search(
                "articles",
                SearchRequest::new(Query::match_all())
                    .sort(Sort::field("status", SortOrder::Asc))
                    .limit(10),
            )
            .await
            .unwrap();
        let statuses: Vec<_> = results
            .hits
            .iter()
            .map(|h| {
                h.document
                    .get("status")
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let mut sorted = statuses.clone();
        sorted.sort();
        assert_eq!(statuses, sorted);
    }

    #[tokio::test]
    async fn delete_removes_document_from_results() {
        let backend = seeded_backend().await;
        backend.delete("articles", "1").await.unwrap();
        backend.commit("articles").await.unwrap();
        let results = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap();
        assert_eq!(results.total, 2);
        assert!(results.hits.iter().all(|h| h.id != "1"));
    }

    #[tokio::test]
    async fn reindexing_same_id_replaces_document() {
        let backend = seeded_backend().await;
        backend
            .index(
                "articles",
                Document::new()
                    .with_id("1")
                    .set("title", "Completely different")
                    .set("status", "archived")
                    .set("views", 5),
            )
            .await
            .unwrap();
        backend.commit("articles").await.unwrap();
        let results = backend
            .search("articles", Query::term("status", "archived").into())
            .await
            .unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.hits[0].id, "1");
    }

    #[tokio::test]
    async fn on_disk_backend_persists_within_process() {
        let dir = tempfile::tempdir().unwrap();
        let backend = TantivyBackend::on_disk(dir.path());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
            .index(
                "articles",
                Document::new().with_id("1").set("title", "hello disk"),
            )
            .await
            .unwrap();
        backend.commit("articles").await.unwrap();

        let results = backend
            .search("articles", Query::match_query("title", "hello").into())
            .await
            .unwrap();
        assert_eq!(results.total, 1);
    }
}
