//! A [`SearchBackend`] implementation backed by a remote
//! [Elasticsearch](https://www.elastic.co/elasticsearch) cluster, talked
//! to over HTTP via [`reqwest`]. (OpenSearch speaks this same wire
//! protocol too - see `rusty-search-opensearch`, which wraps this crate
//! rather than reimplementing it.)
//!
//! Unlike `rusty-search-memory`/`rusty-search-tantivy`, this backend has no
//! in-process index of its own - `ElasticsearchBackend` is a thin client.
//! It still has to track a little state locally: which indices it created
//! and each one's field types, so query translation can coerce a `Query`'s
//! string/JSON values into the right JSON type without a round trip to
//! fetch the mapping back from the cluster.
//!
//! ## Known limitations
//!
//! - `index_exists`/operations on an unknown index reflect this backend's
//!   own local registry (indices created through this `SearchBackend`
//!   instance), not the cluster's actual state - an index created by some
//!   other client won't be visible here. This matches the other backends'
//!   model, where index lifecycle is expected to go entirely through the
//!   trait.
//! - `FieldOptions::fast` has no effect: Elasticsearch already keeps
//!   doc_values (its equivalent of a Tantivy "fast field") for `keyword`,
//!   numeric, boolean, and `date` fields by default, so every sortable core
//!   field type is already sortable here.
//! - Unlike `rusty-search-tantivy`, `Query::Bool`'s `filter` clauses really
//!   are non-scoring here - they map directly onto Elasticsearch's `bool`
//!   query `filter` context.

mod convert;
mod query_map;
mod schema_map;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, Hit, Result, Schema as CoreSchema, SearchBackend, SearchError, SearchRequest,
    SearchResults, Sort, SortOrder,
};

use convert::{document_to_source, source_to_document};
use schema_map::{build_index_body, FieldMap};

#[derive(Clone)]
enum Auth {
    None,
    Basic { username: String, password: String },
    ApiKey(String),
}

/// An Elasticsearch/OpenSearch-backed [`SearchBackend`]. Cheaply
/// cloneable - clones share the same HTTP client and index registry.
#[derive(Clone)]
pub struct ElasticsearchBackend {
    client: reqwest::Client,
    base_url: String,
    auth: Auth,
    indices: Arc<RwLock<HashMap<String, FieldMap>>>,
}

impl ElasticsearchBackend {
    /// Connects to an unauthenticated cluster (e.g. a local dev instance)
    /// at `base_url` (e.g. `"http://localhost:9200"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client_and_auth(base_url, reqwest::Client::new(), Auth::None)
    }

    /// Connects using HTTP basic auth.
    pub fn with_basic_auth(
        base_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self::with_client_and_auth(
            base_url,
            reqwest::Client::new(),
            Auth::Basic {
                username: username.into(),
                password: password.into(),
            },
        )
    }

    /// Connects using an Elasticsearch API key (sent as `Authorization:
    /// ApiKey <key>`).
    pub fn with_api_key(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client_and_auth(
            base_url,
            reqwest::Client::new(),
            Auth::ApiKey(api_key.into()),
        )
    }

    /// Connects with a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts, TLS config, proxies, etc), unauthenticated beyond whatever
    /// the client itself is configured with.
    pub fn with_client(base_url: impl Into<String>, client: reqwest::Client) -> Self {
        Self::with_client_and_auth(base_url, client, Auth::None)
    }

    fn with_client_and_auth(
        base_url: impl Into<String>,
        client: reqwest::Client,
        auth: Auth,
    ) -> Self {
        let base_url = base_url.into();
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth,
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .request(method, format!("{}/{path}", self.base_url));
        match &self.auth {
            Auth::None => builder,
            Auth::Basic { username, password } => builder.basic_auth(username, Some(password)),
            Auth::ApiKey(key) => builder.header("Authorization", format!("ApiKey {key}")),
        }
    }

    async fn require_known(&self, index: &str) -> Result<FieldMap> {
        self.indices
            .read()
            .await
            .get(index)
            .cloned()
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))
    }
}

fn backend_err(e: impl std::error::Error + Send + Sync + 'static) -> SearchError {
    SearchError::Backend(anyhow::Error::new(e))
}

async fn error_for_status(resp: reqwest::Response) -> SearchError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    SearchError::Backend(anyhow!("elasticsearch returned {status}: {body}"))
}

#[async_trait]
impl SearchBackend for ElasticsearchBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        if self.indices.read().await.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let (body, fields) = build_index_body(&schema);
        let resp = self
            .request(Method::PUT, name)
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;

        if resp.status() == StatusCode::BAD_REQUEST {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("resource_already_exists_exception") {
                return Err(SearchError::IndexAlreadyExists(name.to_string()));
            }
            return Err(SearchError::Backend(anyhow!(
                "elasticsearch rejected index creation: {body}"
            )));
        }
        if !resp.status().is_success() {
            return Err(error_for_status(resp).await);
        }

        self.indices.write().await.insert(name.to_string(), fields);
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        self.require_known(name).await?;

        let resp = self
            .request(Method::DELETE, name)
            .send()
            .await
            .map_err(backend_err)?;
        if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
            return Err(error_for_status(resp).await);
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

        let mut body = String::new();
        for document in documents {
            let (id, source) = document_to_source(document);
            let action = json!({ "index": { "_index": index, "_id": id } });
            body.push_str(&action.to_string());
            body.push('\n');
            body.push_str(&source.to_string());
            body.push('\n');
        }

        let resp = self
            .request(Method::POST, "_bulk")
            .header("Content-Type", "application/x-ndjson")
            .body(body)
            .send()
            .await
            .map_err(backend_err)?;
        if !resp.status().is_success() {
            return Err(error_for_status(resp).await);
        }

        let parsed: Value = resp.json().await.map_err(backend_err)?;
        if parsed
            .get("errors")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(SearchError::Backend(anyhow!(
                "elasticsearch bulk request reported per-item errors: {parsed}"
            )));
        }
        Ok(())
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.require_known(index).await?;

        let resp = self
            .request(Method::DELETE, &format!("{index}/_doc/{id}"))
            .send()
            .await
            .map_err(backend_err)?;
        if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(error_for_status(resp).await)
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let fields = self.require_known(index).await?;
        let query_body = query_map::build_query(&request.query, &fields)?;

        let mut body = json!({
            "query": query_body,
            "from": request.offset,
            "size": request.limit,
            "track_total_hits": true,
            "track_scores": true,
        });
        if !request.sort.is_empty() {
            let sort: Vec<Value> = request.sort.iter().map(sort_to_es).collect();
            body["sort"] = Value::Array(sort);
        }

        let resp = self
            .request(Method::POST, &format!("{index}/_search"))
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(SearchError::IndexNotFound(index.to_string()));
        }
        if !resp.status().is_success() {
            return Err(error_for_status(resp).await);
        }

        let parsed: Value = resp.json().await.map_err(backend_err)?;
        parse_search_response(parsed)
    }

    async fn commit(&self, index: &str) -> Result<()> {
        self.require_known(index).await?;

        let resp = self
            .request(Method::POST, &format!("{index}/_refresh"))
            .send()
            .await
            .map_err(backend_err)?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(SearchError::IndexNotFound(index.to_string()));
        }
        if !resp.status().is_success() {
            return Err(error_for_status(resp).await);
        }
        Ok(())
    }
}

fn sort_to_es(sort: &Sort) -> Value {
    match sort {
        Sort::Score => json!({ "_score": { "order": "desc" } }),
        Sort::Field { name, order } => {
            let order_str = match order {
                SortOrder::Asc => "asc",
                SortOrder::Desc => "desc",
            };
            let mut object = Map::new();
            object.insert(name.clone(), json!({ "order": order_str }));
            Value::Object(object)
        }
    }
}

fn parse_search_response(parsed: Value) -> Result<SearchResults> {
    let hits_obj = parsed.get("hits").ok_or_else(|| {
        SearchError::Backend(anyhow!("malformed elasticsearch response: missing `hits`"))
    })?;

    let total = hits_obj
        .get("total")
        .and_then(|t| t.get("value"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let hits = hits_obj
        .get("hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|hit| {
            let id = hit
                .get("_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let score = hit.get("_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let source = hit.get("_source").cloned().unwrap_or(Value::Null);
            Hit {
                id: id.clone(),
                score,
                document: source_to_document(id, source),
            }
        })
        .collect();

    Ok(SearchResults { hits, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{Query, Schema};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field("views")
            .build()
    }

    #[tokio::test]
    async fn create_index_sends_mapping_and_registers_locally() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(&server)
            .await;

        let backend = ElasticsearchBackend::new(server.uri());
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
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ElasticsearchBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
        // `.expect(1)` above is checked on drop - a second HTTP call would fail it.
    }

    #[tokio::test]
    async fn create_index_maps_es_conflict_response_to_already_exists() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": { "type": "resource_already_exists_exception", "reason": "index already exists" }
            })))
            .mount(&server)
            .await;

        let backend = ElasticsearchBackend::new(server.uri());
        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        // No mocks registered at all - if any of these made a request, they'd
        // get a connection error, not `IndexNotFound`.
        let server = MockServer::start().await;
        let backend = ElasticsearchBackend::new(server.uri());

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

    async fn backend_with_articles_index(server: &MockServer) -> ElasticsearchBackend {
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(server)
            .await;
        let backend = ElasticsearchBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_sends_bulk_ndjson() {
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
                    .set("title", "Rust async search")],
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let bulk_request = requests
            .iter()
            .find(|r| r.url.path() == "/_bulk")
            .expect("bulk request was sent");
        let body = String::from_utf8(bulk_request.body.clone()).unwrap();
        let mut lines = body.lines();
        let action: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(action["index"]["_index"], "articles");
        assert_eq!(action["index"]["_id"], "1");
        let source: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(source["title"], "Rust async search");
    }

    #[tokio::test]
    async fn index_batch_surfaces_per_item_bulk_errors() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/_bulk"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": true,
                "items": [{ "index": { "status": 400, "error": { "type": "mapper_parsing_exception" } } }]
            })))
            .mount(&server)
            .await;

        let err = backend
            .index_batch("articles", vec![Document::new().with_id("1")])
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::Backend(_)));
    }

    #[tokio::test]
    async fn delete_treats_404_as_a_no_op() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/articles/_doc/missing-id"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        backend.delete("articles", "missing-id").await.unwrap();
    }

    #[tokio::test]
    async fn commit_calls_refresh() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/articles/_refresh"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "_shards": { "successful": 1 } })),
            )
            .mount(&server)
            .await;

        backend.commit("articles").await.unwrap();
    }

    #[tokio::test]
    async fn search_sends_translated_query_and_parses_hits() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        let expected_body = json!({
            "query": { "term": { "status": "published" } },
            "from": 0,
            "size": 10,
            "track_total_hits": true,
            "track_scores": true,
        });
        Mock::given(method("POST"))
            .and(path("/articles/_search"))
            .and(body_json(&expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {
                    "total": { "value": 1, "relation": "eq" },
                    "hits": [
                        { "_id": "1", "_score": 1.5, "_source": { "title": "Rust async search", "status": "published" } }
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
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].id, "1");
        assert_eq!(results.hits[0].score, 1.5);
        assert_eq!(
            results.hits[0].document.get("title").unwrap(),
            "Rust async search"
        );
    }

    #[tokio::test]
    async fn search_maps_an_es_side_404_to_index_not_found() {
        // Covers the cluster's view diverging from our local registry (e.g.
        // the index was dropped through some other client).
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/articles/_search"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "error": { "type": "index_not_found_exception" }
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
    async fn basic_auth_header_is_sent() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/articles"))
            .and(wiremock::matchers::basic_auth("user", "pass"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "acknowledged": true })))
            .mount(&server)
            .await;

        let backend = ElasticsearchBackend::with_basic_auth(server.uri(), "user", "pass");
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }
}
