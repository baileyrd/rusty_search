use async_trait::async_trait;

use crate::document::Document;
use crate::error::Result;
use crate::query::SearchRequest;
use crate::result::SearchResults;
use crate::schema::Schema;

/// The standard interface every search engine plugs into.
///
/// This plays the role SQLAlchemy's `Dialect`/`Engine` pair plays for
/// databases: application code is written once against `SearchBackend`, and
/// the concrete engine underneath - an in-memory index, an embedded Tantivy
/// index, a remote Elasticsearch/Meilisearch cluster - is an implementation
/// detail selected at construction time and swappable without touching call
/// sites.
///
/// The trait is written with `#[async_trait]` specifically so it stays
/// object-safe: applications can hold a `Arc<dyn SearchBackend>` and swap
/// the concrete engine at runtime (e.g. in-memory in tests, Tantivy in
/// production), exactly as they'd swap a SQLAlchemy engine's connection
/// string.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    /// Creates a new index with the given field layout.
    ///
    /// Returns [`crate::SearchError::IndexAlreadyExists`] if an index with
    /// this name already exists.
    async fn create_index(&self, name: &str, schema: Schema) -> Result<()>;

    /// Deletes an index and all of its documents.
    ///
    /// Returns [`crate::SearchError::IndexNotFound`] if it doesn't exist.
    async fn delete_index(&self, name: &str) -> Result<()>;

    /// Returns whether an index with this name exists.
    async fn index_exists(&self, name: &str) -> Result<bool>;

    /// Indexes (inserts or replaces) a single document.
    ///
    /// A document without an `id` is assigned one by the backend; the
    /// generated id is not currently returned by this method, so callers
    /// that need it back should set `id` themselves before calling.
    async fn index(&self, index: &str, document: Document) -> Result<()> {
        self.index_batch(index, vec![document]).await
    }

    /// Indexes (inserts or replaces) a batch of documents. This is the
    /// primitive backends implement; [`SearchBackend::index`] is a
    /// single-document convenience built on top of it.
    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()>;

    /// Removes a document by id. Backends should treat deleting a
    /// nonexistent document as a no-op rather than an error, matching most
    /// search engines' semantics.
    async fn delete(&self, index: &str, id: &str) -> Result<()>;

    /// Runs a search against an index.
    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults>;

    /// Makes previously indexed/deleted documents visible to subsequent
    /// searches. Backends with immediate read-after-write consistency may
    /// implement this as a no-op.
    async fn commit(&self, index: &str) -> Result<()>;
}
