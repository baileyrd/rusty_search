//! A [`SearchBackend`] implementation backed by a remote
//! [OpenSearch](https://opensearch.org) cluster.
//!
//! OpenSearch is a fork of Elasticsearch (pre-7.11) and, for every
//! operation this workspace's `SearchBackend` trait needs - index
//! creation and mappings, bulk indexing, document deletion, refresh, and
//! the Query DSL used for search - it still speaks the same wire
//! protocol today. Rather than duplicating `rusty-search-elasticsearch`'s
//! request/response translation (and its test suite) against an
//! effectively identical API, this crate wraps
//! [`ElasticsearchBackend`](rusty_search_elasticsearch::ElasticsearchBackend)
//! and delegates every [`SearchBackend`] method to it. See
//! `docs/adr/0004-opensearch-backend-as-a-wrapper.md` in the workspace
//! root for the full reasoning and its tradeoffs.
//!
//! ## Known limitations
//!
//! - Every limitation documented on `rusty-search-elasticsearch` applies
//!   here too (its module docs cover them), since this crate delegates to
//!   it entirely.
//! - No first-class AWS SigV4 request signing for Amazon OpenSearch
//!   Service - the common way to reach a *managed* OpenSearch cluster on
//!   AWS. [`OpenSearchBackend::with_client`] accepts a pre-configured
//!   [`reqwest::Client`], which is the escape hatch until this crate adds
//!   direct support (e.g. a signing mechanism); a caller can layer SigV4
//!   signing into that client themselves in the meantime.

use async_trait::async_trait;
use rusty_search_core::{Document, Result, Schema, SearchBackend, SearchRequest, SearchResults};
use rusty_search_elasticsearch::ElasticsearchBackend;

/// An OpenSearch-backed [`SearchBackend`]. A thin wrapper around
/// [`ElasticsearchBackend`] - see the module docs for why. Cheaply
/// cloneable, same as the backend it wraps.
#[derive(Clone)]
pub struct OpenSearchBackend(ElasticsearchBackend);

impl OpenSearchBackend {
    /// Connects to an unauthenticated cluster (e.g. a local dev instance
    /// with the security plugin disabled) at `base_url` (e.g.
    /// `"http://localhost:9200"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self(ElasticsearchBackend::new(base_url))
    }

    /// Connects using HTTP basic auth - OpenSearch's security plugin's
    /// default authentication method (e.g. the bundled `admin` user).
    pub fn with_basic_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self(ElasticsearchBackend::with_basic_auth(
            base_url, username, password,
        ))
    }

    /// Connects with a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts, TLS config, proxies, or - notably - injecting AWS SigV4
    /// signing for Amazon OpenSearch Service via the client's middleware,
    /// which this crate doesn't implement directly yet).
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        Self(ElasticsearchBackend::with_client(base_url, client))
    }
}

#[async_trait]
impl SearchBackend for OpenSearchBackend {
    async fn create_index(&self, name: &str, schema: Schema) -> Result<()> {
        self.0.create_index(name, schema).await
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        self.0.delete_index(name).await
    }

    async fn index_exists(&self, name: &str) -> Result<bool> {
        self.0.index_exists(name).await
    }

    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()> {
        self.0.index_batch(index, documents).await
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.0.delete(index, id).await
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        self.0.search(index, request).await
    }

    async fn commit(&self, index: &str) -> Result<()> {
        self.0.commit(index).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Query, SearchError};
    use serde_json::json;
    use wiremock::matchers::{basic_auth, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // This suite proves the *delegation* is wired correctly - construction,
    // request round trips, and error mapping all actually reach
    // `ElasticsearchBackend` through the wrapper. It deliberately doesn't
    // re-verify query/schema/document translation edge cases: those are
    // `rusty-search-elasticsearch`'s own logic, already covered by its test
    // suite, and identical here since this crate delegates to it entirely.

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    #[tokio::test]
    async fn create_index_reaches_the_server_through_the_wrapper() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(&server)
            .await;

        let backend = OpenSearchBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        assert!(backend.index_exists("articles").await.unwrap());
    }

    #[tokio::test]
    async fn create_index_maps_a_conflict_response_to_already_exists() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": { "type": "resource_already_exists_exception", "reason": "index already exists" }
            })))
            .mount(&server)
            .await;

        let backend = OpenSearchBackend::new(server.uri());
        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        let server = MockServer::start().await;
        let backend = OpenSearchBackend::new(server.uri());

        let err = backend
            .search("missing", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "missing"));
    }

    async fn backend_with_articles_index(server: &MockServer) -> OpenSearchBackend {
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(server)
            .await;
        let backend = OpenSearchBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_search_delete_and_commit_round_trip_through_the_wrapper() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/_bulk"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "errors": false, "items": [] })),
            )
            .mount(&server)
            .await;
        backend
            .index_batch(
                "articles",
                vec![Document::new()
                    .with_id("1")
                    .set("title", "OpenSearch via the wrapper")
                    .set("status", "published")],
            )
            .await
            .unwrap();

        Mock::given(method("POST"))
            .and(path("/articles/_refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "_shards": { "successful": 1 } })),
            )
            .mount(&server)
            .await;
        backend.commit("articles").await.unwrap();

        Mock::given(method("POST"))
            .and(path("/articles/_search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {
                    "total": { "value": 1, "relation": "eq" },
                    "hits": [
                        { "_id": "1", "_score": 1.0, "_source": { "title": "OpenSearch via the wrapper", "status": "published" } }
                    ]
                }
            })))
            .mount(&server)
            .await;
        let results = backend
            .search("articles", Query::term("status", "published").into())
            .await
            .unwrap();
        assert_eq!(results.total, 1);
        assert_eq!(results.hits[0].id, "1");

        Mock::given(method("DELETE"))
            .and(path("/articles/_doc/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "result": "deleted" })))
            .mount(&server)
            .await;
        backend.delete("articles", "1").await.unwrap();
    }

    #[tokio::test]
    async fn with_basic_auth_sends_the_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .and(basic_auth("admin", "admin"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(&server)
            .await;

        let backend = OpenSearchBackend::with_basic_auth(server.uri(), "admin", "admin");
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn with_client_uses_the_supplied_client() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let backend = OpenSearchBackend::with_client(server.uri(), client);
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }
}
