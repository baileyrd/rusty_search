//! A [`SearchBackend`] implementation backed by [Algolia](https://www.algolia.com),
//! a hosted search SaaS, talked to over HTTP via [`reqwest`]. No official
//! or well-established async Algolia Rust client was available to build
//! on with confidence, so - like `rusty-search-elasticsearch` and
//! `rusty-search-solr` - this backend hand-rolls the REST API directly.
//!
//! Like `rusty-search-meilisearch`, Algolia's write operations (settings
//! updates, batch indexing, deletion) are asynchronous tasks; this
//! backend waits for each one to finish before its `SearchBackend` method
//! returns, which makes [`SearchBackend::commit`] a no-op - there's
//! nothing left to flush by the time it's called. It also keeps the same
//! kind of local index/field-type registry as the other remote backends,
//! for the same reason (Algolia's per-record schema-less model has no
//! field-type system of its own to query back).
//!
//! ## Known limitations
//!
//! - **No native per-query field sort.** Algolia's model for a custom sort
//!   order is a *replica index* configured with different ranking
//!   settings, which this backend doesn't manage. A `SearchRequest` with
//!   any [`Sort::Field`] instead fetches a bounded candidate set (see
//!   [`FALLBACK_SORT_CAP`]) and sorts it in memory - correct up to that
//!   cap, not beyond it, and less efficient than a real replica-backed
//!   sort would be.
//! - **No relevance score.** Algolia doesn't expose a single aggregate
//!   relevance score the way Elasticsearch/Solr/Tantivy do (only detailed,
//!   multi-criteria ranking metadata on request, with no natural single
//!   float). Every [`Hit::score`] from this backend is a constant `1.0`;
//!   result *order* (not the score value) reflects Algolia's actual
//!   ranking.
//! - **No multi-host failover.** Official Algolia clients retry across a
//!   list of fallback DSN hosts on network errors; this backend talks to
//!   a single derived write host and a single derived read host only.
//! - Like `rusty-search-elasticsearch`, `index_exists`/index-not-found
//!   errors reflect this backend's own local registry of indices it
//!   created, not Algolia's actual state.
//! - `Query::Range` is only supported on `I64`/`F64` fields - Algolia's
//!   filter comparisons require numeric values, not date strings.
//! - `must_not` wrapping a bare `Query::MatchAll`/`Query::Match` isn't
//!   representable and returns `SearchError::InvalidQuery`: unlike
//!   `rusty-search-solr`'s Lucene syntax, Algolia's filter language has no
//!   "match everything" literal to negate against. See [`query_map`] for
//!   the full translation.

mod convert;
pub mod query_map;
mod schema_map;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, Hit, Result, Schema as CoreSchema, SearchBackend, SearchError, SearchRequest,
    SearchResults, Sort, SortOrder,
};

use convert::{document_to_json, json_to_document};
use schema_map::{build_settings, FieldMap};

/// Search results beyond Algolia's native relevance order fall back to
/// sorting an in-memory candidate set capped at this many top-ranked
/// records - see the crate's "no native per-query field sort" limitation.
pub const FALLBACK_SORT_CAP: usize = 10_000;

const TASK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TASK_POLL_MAX_ATTEMPTS: usize = 100;

/// An Algolia-backed [`SearchBackend`]. Cheaply cloneable - clones share
/// the same HTTP client and index registry.
#[derive(Clone)]
pub struct AlgoliaBackend {
    client: reqwest::Client,
    app_id: String,
    api_key: String,
    write_host: String,
    read_host: String,
    indices: Arc<RwLock<HashMap<String, FieldMap>>>,
}

impl AlgoliaBackend {
    /// Connects using an Algolia Application ID and API key (an Admin key
    /// for indexing, or a Search key if only searching). Hosts are
    /// derived from `app_id` per Algolia's standard scheme:
    /// `https://{app_id}.algolia.net` for writes,
    /// `https://{app_id}-dsn.algolia.net` for reads.
    pub fn new(app_id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client(app_id, api_key, reqwest::Client::new())
    }

    /// Connects with a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts, TLS config, proxies, etc), using the standard derived
    /// hosts.
    pub fn with_client(
        app_id: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let app_id = app_id.into();
        let write_host = format!("https://{app_id}.algolia.net");
        let read_host = format!("https://{app_id}-dsn.algolia.net");
        Self::with_hosts_and_client(app_id, api_key, write_host, read_host, client)
    }

    /// Connects with explicit write/read hosts instead of the ones
    /// standard Algolia deploys derive from `app_id` - primarily useful
    /// for pointing both roles at a single test server.
    pub fn with_hosts(
        app_id: impl Into<String>,
        api_key: impl Into<String>,
        write_host: impl Into<String>,
        read_host: impl Into<String>,
    ) -> Self {
        Self::with_hosts_and_client(
            app_id,
            api_key,
            write_host,
            read_host,
            reqwest::Client::new(),
        )
    }

    fn with_hosts_and_client(
        app_id: impl Into<String>,
        api_key: impl Into<String>,
        write_host: impl Into<String>,
        read_host: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            client,
            app_id: app_id.into(),
            api_key: api_key.into(),
            write_host: write_host.into(),
            read_host: read_host.into(),
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn request(&self, method: Method, host: &str, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{host}/{path}"))
            .header("X-Algolia-Application-Id", &self.app_id)
            .header("X-Algolia-API-Key", &self.api_key)
    }

    async fn require_known(&self, index: &str) -> Result<FieldMap> {
        self.indices
            .read()
            .await
            .get(index)
            .cloned()
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))
    }

    async fn parse_response(&self, resp: reqwest::Response, index: &str) -> Result<Value> {
        let status = resp.status();
        let text = resp.text().await.map_err(backend_err)?;

        if status == StatusCode::NOT_FOUND {
            return Err(SearchError::IndexNotFound(index.to_string()));
        }
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| text.clone());
            return Err(SearchError::Backend(anyhow!(
                "algolia returned {status}: {message}"
            )));
        }

        serde_json::from_str(&text).map_err(|e| SearchError::Backend(anyhow!(e)))
    }

    /// Waits for an Algolia indexing task to finish, matching this
    /// crate's SearchBackend methods to the synchronous read-your-writes
    /// contract the trait expects.
    async fn wait_task(&self, index: &str, task_id: u64) -> Result<()> {
        for _ in 0..TASK_POLL_MAX_ATTEMPTS {
            let resp = self
                .request(
                    Method::GET,
                    &self.write_host,
                    &format!("1/indexes/{index}/task/{task_id}"),
                )
                .send()
                .await
                .map_err(backend_err)?;
            let json = self.parse_response(resp, index).await?;
            if json.get("status").and_then(Value::as_str) == Some("published") {
                return Ok(());
            }
            tokio::time::sleep(TASK_POLL_INTERVAL).await;
        }
        Err(SearchError::Backend(anyhow!(
            "timed out waiting for algolia task {task_id} on index `{index}`"
        )))
    }
}

fn backend_err(e: impl std::error::Error + Send + Sync + 'static) -> SearchError {
    SearchError::Backend(anyhow::Error::new(e))
}

#[async_trait]
impl SearchBackend for AlgoliaBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        if self.indices.read().await.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let (settings, fields) = build_settings(&schema);
        let resp = self
            .request(
                Method::PUT,
                &self.write_host,
                &format!("1/indexes/{name}/settings"),
            )
            .json(&settings)
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, name).await?;
        if let Some(task_id) = json.get("taskID").and_then(Value::as_u64) {
            self.wait_task(name, task_id).await?;
        }

        self.indices.write().await.insert(name.to_string(), fields);
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        self.require_known(name).await?;

        let resp = self
            .request(
                Method::DELETE,
                &self.write_host,
                &format!("1/indexes/{name}"),
            )
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, name).await?;
        if let Some(task_id) = json.get("taskID").and_then(Value::as_u64) {
            self.wait_task(name, task_id).await?;
        }

        self.indices.write().await.remove(name);
        Ok(())
    }

    async fn index_exists(&self, name: &str) -> Result<bool> {
        Ok(self.indices.read().await.contains_key(name))
    }

    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()> {
        self.require_known(index).await?;
        if documents.is_empty() {
            return Ok(());
        }

        let requests: Vec<Value> = documents
            .into_iter()
            .map(|doc| {
                let (_, body) = document_to_json(doc);
                json!({ "action": "updateObject", "body": body })
            })
            .collect();

        let resp = self
            .request(
                Method::POST,
                &self.write_host,
                &format!("1/indexes/{index}/batch"),
            )
            .json(&json!({ "requests": requests }))
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, index).await?;
        if let Some(task_id) = json.get("taskID").and_then(Value::as_u64) {
            self.wait_task(index, task_id).await?;
        }
        Ok(())
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.require_known(index).await?;

        let resp = self
            .request(
                Method::DELETE,
                &self.write_host,
                &format!("1/indexes/{index}/{id}"),
            )
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, index).await?;
        if let Some(task_id) = json.get("taskID").and_then(Value::as_u64) {
            self.wait_task(index, task_id).await?;
        }
        Ok(())
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let fields = self.require_known(index).await?;
        let params = query_map::build_search_params(&request.query, &fields)?;

        let mut body = json!({ "query": params.query });
        if let Some(attr) = &params.restrict_searchable_attributes {
            body["restrictSearchableAttributes"] = json!([attr]);
        }
        if let Some(filters) = &params.filters {
            body["filters"] = json!(filters);
        }

        let needs_fallback_sort = request.sort.iter().any(|s| matches!(s, Sort::Field { .. }));
        if needs_fallback_sort {
            let cap = FALLBACK_SORT_CAP.max(request.offset + request.limit);
            body["offset"] = json!(0);
            body["length"] = json!(cap);
        } else {
            body["offset"] = json!(request.offset);
            body["length"] = json!(request.limit);
        }

        let resp = self
            .request(
                Method::POST,
                &self.read_host,
                &format!("1/indexes/{index}/query"),
            )
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, index).await?;

        let total = json.get("nbHits").and_then(Value::as_u64).unwrap_or(0) as usize;
        let mut scored: Vec<(f32, Document)> = json
            .get("hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|hit| (1.0f32, json_to_document(hit)))
            .collect();

        if needs_fallback_sort {
            sort_in_memory(&mut scored, &request.sort);
            scored = scored
                .into_iter()
                .skip(request.offset)
                .take(request.limit)
                .collect();
        }

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
        // Every write above already waits for its Algolia task to finish,
        // so there is nothing left to flush by the time `commit` is called.
        self.require_known(index).await?;
        Ok(())
    }
}

fn sort_in_memory(scored: &mut [(f32, Document)], sorts: &[Sort]) {
    if sorts.is_empty() {
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
    use rusty_search_core::{FieldOptions, Query, Schema, SortOrder};
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    fn backend(server: &MockServer) -> AlgoliaBackend {
        AlgoliaBackend::with_hosts("test-app", "test-key", server.uri(), server.uri())
    }

    async fn mock_published_task(server: &MockServer, task_id: u64) {
        Mock::given(method("GET"))
            .and(path(format!("/1/indexes/articles/task/{task_id}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status": "published" })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn create_index_sends_settings_and_registers_locally() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/1/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "taskID": 1 })))
            .mount(&server)
            .await;
        mock_published_task(&server, 1).await;

        let backend = backend(&server);
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        assert!(backend.index_exists("articles").await.unwrap());
    }

    #[tokio::test]
    async fn create_index_rejects_duplicates_without_a_second_request() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/1/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "taskID": 1 })))
            .expect(1)
            .mount(&server)
            .await;
        mock_published_task(&server, 1).await;

        let backend = backend(&server);
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        let server = MockServer::start().await;
        let backend = backend(&server);

        assert!(matches!(
            backend.search("missing", Query::match_all().into()).await.unwrap_err(),
            SearchError::IndexNotFound(name) if name == "missing"
        ));
        assert!(matches!(
            backend.delete("missing", "1").await.unwrap_err(),
            SearchError::IndexNotFound(name) if name == "missing"
        ));
        assert!(matches!(
            backend.commit("missing").await.unwrap_err(),
            SearchError::IndexNotFound(name) if name == "missing"
        ));
        assert!(matches!(
            backend.delete_index("missing").await.unwrap_err(),
            SearchError::IndexNotFound(name) if name == "missing"
        ));
    }

    async fn backend_with_articles_index(server: &MockServer) -> AlgoliaBackend {
        Mock::given(method("PUT"))
            .and(path("/1/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "taskID": 1 })))
            .mount(server)
            .await;
        mock_published_task(server, 1).await;

        let backend = backend(server);
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_sends_update_object_commands() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/1/indexes/articles/batch"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "taskID": 2, "objectIDs": ["1"] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/1/indexes/articles/task/2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status": "published" })),
            )
            .mount(&server)
            .await;

        backend
            .index_batch(
                "articles",
                vec![Document::new()
                    .with_id("1")
                    .set("title", "Rust async search")],
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let batch = requests
            .iter()
            .find(|r| r.url.path() == "/1/indexes/articles/batch")
            .expect("batch request was sent");
        let body: Value = serde_json::from_slice(&batch.body).unwrap();
        assert_eq!(body["requests"][0]["action"], "updateObject");
        assert_eq!(body["requests"][0]["body"]["objectID"], "1");
        assert_eq!(body["requests"][0]["body"]["title"], "Rust async search");
    }

    #[tokio::test]
    async fn delete_sends_a_delete_request() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/1/indexes/articles/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "taskID": 3 })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/1/indexes/articles/task/3"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "status": "published" })),
            )
            .mount(&server)
            .await;

        backend.delete("articles", "1").await.unwrap();
    }

    #[tokio::test]
    async fn commit_makes_no_http_call() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;
        // No mocks registered beyond index creation - if commit made any
        // request, it would hit an unmocked route and fail.
        backend.commit("articles").await.unwrap();
    }

    #[tokio::test]
    async fn search_sends_translated_query_and_parses_hits() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/1/indexes/articles/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nbHits": 1,
                "hits": [
                    { "objectID": "1", "title": "Rust async search", "status": "published" }
                ]
            })))
            .mount(&server)
            .await;

        let results = backend
            .search("articles", Query::term("status", "published").into())
            .await
            .unwrap();

        assert_eq!(results.total, 1);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].id, "1");
        assert_eq!(results.hits[0].score, 1.0);
        assert_eq!(
            results.hits[0].document.get("title").unwrap(),
            "Rust async search"
        );

        let requests = server.received_requests().await.unwrap();
        let query_req = requests
            .iter()
            .find(|r| r.url.path() == "/1/indexes/articles/query")
            .expect("query request was sent");
        let body: Value = serde_json::from_slice(&query_req.body).unwrap();
        assert_eq!(body["filters"], "status:\"published\"");
    }

    #[tokio::test]
    async fn search_maps_a_404_to_index_not_found() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/1/indexes/articles/query"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "message": "Index articles does not exist", "status": 404
            })))
            .mount(&server)
            .await;

        let err = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "articles"));
    }

    #[tokio::test]
    async fn requests_include_algolia_auth_headers() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/1/indexes/articles/settings"))
            .and(header("X-Algolia-Application-Id", "test-app"))
            .and(header("X-Algolia-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "taskID": 1 })))
            .mount(&server)
            .await;
        mock_published_task(&server, 1).await;

        let backend = backend(&server);
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn fallback_sort_orders_results_by_field() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/1/indexes/articles/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "nbHits": 3,
                "hits": [
                    { "objectID": "1", "views": 100 },
                    { "objectID": "2", "views": 10 },
                    { "objectID": "3", "views": 50 }
                ]
            })))
            .mount(&server)
            .await;

        let results = backend
            .search(
                "articles",
                SearchRequest::new(Query::match_all()).sort(Sort::field("views", SortOrder::Asc)),
            )
            .await
            .unwrap();

        let ids: Vec<_> = results.hits.iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids, vec!["2", "3", "1"]);
        assert_eq!(results.total, 3);
    }
}
