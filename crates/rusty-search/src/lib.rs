//! `rusty_search`: an async, pluggable search interface for Rust.
//!
//! In the spirit of SQLAlchemy's `Engine`/dialect split for databases,
//! application code is written once against [`SearchBackend`] and the
//! concrete search engine underneath is chosen - and swappable - at
//! construction time:
//!
//! ```
//! # #[cfg(feature = "memory")]
//! # #[tokio::main]
//! # async fn main() -> rusty_search::Result<()> {
//! use std::sync::Arc;
//! use rusty_search::{Document, Query, Schema, SearchBackend};
//!
//! // Swap this for `rusty_search::TantivyBackend::in_memory()` (or your own
//! // `SearchBackend` impl) and every line below stays the same.
//! let backend: Arc<dyn SearchBackend> = Arc::new(rusty_search::MemoryBackend::new());
//!
//! backend.create_index("articles", Schema::builder().text("title").build()).await?;
//! backend.index("articles", Document::new().with_id("1").set("title", "Rust async search")).await?;
//! backend.commit("articles").await?;
//!
//! let results = backend.search("articles", Query::match_query("title", "rust").into()).await?;
//! assert_eq!(results.total, 1);
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "memory"))]
//! # fn main() {}
//! ```
//!
//! Enable the `memory` feature for the dependency-free [`MemoryBackend`]
//! (a great default for tests), the `tantivy` feature for the embedded,
//! real full-text-search [`TantivyBackend`], the `elasticsearch` feature
//! for [`ElasticsearchBackend`], a thin HTTP client for a remote
//! Elasticsearch cluster, the `opensearch` feature for
//! [`OpenSearchBackend`] (a thin wrapper around `ElasticsearchBackend`,
//! since OpenSearch still speaks Elasticsearch's wire protocol for
//! everything this crate needs), the `meilisearch` feature for
//! [`MeilisearchBackend`], a client for a remote Meilisearch instance,
//! the `solr` feature for [`SolrBackend`], a client for a remote Apache
//! Solr instance, the `algolia` feature for [`AlgoliaBackend`], a client
//! for the hosted Algolia search SaaS, and/or the `azure-search` feature
//! for [`AzureSearchBackend`], a client for the hosted Azure AI Search
//! service. None are enabled by default so that depending on
//! `rusty-search-core` alone - to define your own backend, or to write
//! backend-agnostic application code - pulls in nothing else.

pub use rusty_search_core::*;

#[cfg(feature = "memory")]
pub use rusty_search_memory::MemoryBackend;

#[cfg(feature = "tantivy")]
pub use rusty_search_tantivy::TantivyBackend;

#[cfg(feature = "elasticsearch")]
pub use rusty_search_elasticsearch::ElasticsearchBackend;

#[cfg(feature = "meilisearch")]
pub use rusty_search_meilisearch::MeilisearchBackend;

#[cfg(feature = "opensearch")]
pub use rusty_search_opensearch::OpenSearchBackend;

#[cfg(feature = "solr")]
pub use rusty_search_solr::SolrBackend;

#[cfg(feature = "algolia")]
pub use rusty_search_algolia::AlgoliaBackend;

#[cfg(feature = "azure-search")]
pub use rusty_search_azure_search::AzureSearchBackend;
