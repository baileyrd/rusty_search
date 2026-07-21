//! A [`SearchBackend`] implementation backed by [Azure AI
//! Search](https://azure.microsoft.com/en-us/products/ai-services/ai-search)
//! (formerly Azure Cognitive Search), a hosted search service on Azure,
//! talked to over HTTP via [`reqwest`]. No official async Azure AI Search
//! Rust SDK with confidence comparable to `meilisearch-sdk`'s was
//! available, so - like `rusty-search-elasticsearch`, `rusty-search-solr`,
//! and `rusty-search-algolia` - this backend hand-rolls the REST API
//! directly.
//!
//! Unlike Meilisearch/Algolia's async task model, Azure AI Search's writes
//! are synchronous over HTTP and indexing is automatically near-real-time,
//! so there's no task to poll - but also nothing for [`SearchBackend::commit`]
//! to flush, making it a no-op here too, for a different reason than either
//! of those two backends. It keeps the same kind of local index/field-type
//! registry as every other remote backend in this workspace, for the usual
//! reason: translating a `Query`/`Sort` without a round trip back to the
//! index definition.
//!
//! ## Known limitations
//!
//! - Like `rusty-search-elasticsearch`, `index_exists`/index-not-found
//!   errors reflect this backend's own local registry of indices it
//!   created, not Azure's actual state.
//! - The mandatory key field is always named `"id"` (not configurable),
//!   and Azure restricts key values to letters, digits, underscore, dash,
//!   and equal sign; a caller-supplied id outside that set isn't validated
//!   client-side and surfaces as a 400 from Azure itself.
//! - `Query::Range` is only supported on `I64`/`F64`/`Date` fields.
//! - `Query::Match` isn't supported inside `Query::Bool::filter` - OData's
//!   filter grammar has no full-text primitive; use `must`/`should`/
//!   `must_not` instead, which translate through the Lucene `search`
//!   parameter. See [`query_map`] for the full translation, including the
//!   narrower `must_not(Query::MatchAll)` restriction outside `filter`.
//! - **Sorting mirrors Tantivy's fast fields, not Elasticsearch's
//!   everything-is-sortable default.** A field is only usable in a native
//!   `$orderby` clause if it was created with `FieldOptions::fast(true)`
//!   (mapped onto Azure's `sortable` attribute, which - like a Tantivy fast
//!   field - must be declared at index-creation time). A `SearchRequest`
//!   sorting by any non-sortable field instead fetches a bounded candidate
//!   set (see [`FALLBACK_SORT_CAP`]) and sorts it in memory, the same
//!   fallback shape `rusty-search-tantivy`/`rusty-search-algolia` use.
//! - No support for Azure Active Directory/managed-identity authentication,
//!   only the simpler `api-key` header, consistent with this workspace's
//!   other hand-rolled HTTP backends' scope.

mod convert;
pub mod query_map;
mod schema_map;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

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
use schema_map::{build_index_body, FieldMap, KEY_FIELD};

/// Azure AI Search REST API version this backend targets.
const API_VERSION: &str = "2023-11-01";

/// Search results sorted by a non-`sortable` field fall back to sorting an
/// in-memory candidate set capped at this many top-ranked records - see the
/// crate's "sorting mirrors Tantivy's fast fields" limitation.
pub const FALLBACK_SORT_CAP: usize = 10_000;

/// An Azure AI Search-backed [`SearchBackend`]. Cheaply cloneable - clones
/// share the same HTTP client and index registry.
#[derive(Clone)]
pub struct AzureSearchBackend {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    indices: Arc<RwLock<HashMap<String, FieldMap>>>,
}

impl AzureSearchBackend {
    /// Connects to an Azure AI Search service at `endpoint` (e.g.
    /// `"https://my-service.search.windows.net"`) using an admin or query
    /// API key.
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client(endpoint, api_key, reqwest::Client::new())
    }

    /// Connects with a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts, TLS config, proxies, etc).
    pub fn with_client(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            client,
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(
                method,
                format!("{}/{path}?api-version={API_VERSION}", self.endpoint),
            )
            .header("api-key", &self.api_key)
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
    SearchError::Backend(anyhow!("azure ai search returned {status}: {body}"))
}

/// Checks an Azure `docs/index` batch response for per-document failures
/// (Azure reports these inside a `200`/`207` body rather than as an HTTP
/// error status).
async fn check_batch_errors(resp: reqwest::Response) -> Result<()> {
    let parsed: Value = resp.json().await.map_err(backend_err)?;
    let failures: Vec<String> = parsed
        .get("value")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("status").and_then(Value::as_bool) == Some(false))
        .map(|item| {
            format!(
                "{}: {}",
                item.get("key").and_then(Value::as_str).unwrap_or("?"),
                item.get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
            )
        })
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(SearchError::Backend(anyhow!(
            "azure ai search reported per-document errors: {}",
            failures.join("; ")
        )))
    }
}

#[async_trait]
impl SearchBackend for AzureSearchBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        if self.indices.read().await.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let (mut body, fields) = build_index_body(&schema);
        body["name"] = json!(name);

        let resp = self
            .request(Method::PUT, &format!("indexes/{name}"))
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;

        if resp.status() == StatusCode::BAD_REQUEST {
            let text = resp.text().await.unwrap_or_default();
            if text.to_lowercase().contains("already exists") {
                return Err(SearchError::IndexAlreadyExists(name.to_string()));
            }
            return Err(SearchError::Backend(anyhow!(
                "azure ai search rejected index creation: {text}"
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
            .request(Method::DELETE, &format!("indexes/{name}"))
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

        let value: Vec<Value> = documents
            .into_iter()
            .map(|doc| {
                let (_id, mut body) = document_to_json(doc);
                body["@search.action"] = json!("upload");
                body
            })
            .collect();

        let resp = self
            .request(Method::POST, &format!("indexes/{index}/docs/index"))
            .json(&json!({ "value": value }))
            .send()
            .await
            .map_err(backend_err)?;
        if !(resp.status().is_success() || resp.status() == StatusCode::MULTI_STATUS) {
            return Err(error_for_status(resp).await);
        }
        check_batch_errors(resp).await
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.require_known(index).await?;

        let body = json!({
            "value": [{ "@search.action": "delete", KEY_FIELD: id }]
        });
        let resp = self
            .request(Method::POST, &format!("indexes/{index}/docs/index"))
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;
        if !(resp.status().is_success() || resp.status() == StatusCode::MULTI_STATUS) {
            return Err(error_for_status(resp).await);
        }
        check_batch_errors(resp).await
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let fields = self.require_known(index).await?;
        let params = query_map::build_search_params(&request.query, &fields)?;

        let mut body = json!({
            "search": params.search,
            "queryType": "full",
            "count": true,
        });
        if let Some(filter) = &params.filter {
            body["filter"] = json!(filter);
        }

        let natively_sortable = request.sort.iter().all(|s| match s {
            Sort::Score => true,
            Sort::Field { name, .. } => fields.get(name).map(|m| m.sortable).unwrap_or(false),
        });

        if natively_sortable {
            body["top"] = json!(request.limit);
            body["skip"] = json!(request.offset);
            if let Some(orderby) = query_map::sort_to_orderby(&request.sort) {
                body["orderby"] = json!(orderby);
            }

            let parsed = self.execute_search(index, &body).await?;
            return parse_search_response(parsed);
        }

        // Fallback: at least one `Sort::Field` references a field that
        // isn't `sortable` - fetch a bounded candidate set in native
        // (relevance) order and sort it in memory, the same shape
        // `rusty-search-tantivy`/`rusty-search-algolia` use.
        body["top"] = json!(FALLBACK_SORT_CAP);
        body["skip"] = json!(0);

        let parsed = self.execute_search(index, &body).await?;
        let mut results = parse_search_response(parsed)?;
        sort_in_memory(&mut results.hits, &request.sort);
        let hits = results
            .hits
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect();
        Ok(SearchResults {
            hits,
            total: results.total,
        })
    }

    async fn commit(&self, index: &str) -> Result<()> {
        // Azure AI Search has no explicit refresh/commit endpoint -
        // indexing is automatically near-real-time, so there's nothing to
        // flush by the time this is called.
        self.require_known(index).await?;
        Ok(())
    }
}

impl AzureSearchBackend {
    async fn execute_search(&self, index: &str, body: &Value) -> Result<Value> {
        let resp = self
            .request(Method::POST, &format!("indexes/{index}/docs/search"))
            .json(body)
            .send()
            .await
            .map_err(backend_err)?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(SearchError::IndexNotFound(index.to_string()));
        }
        if !resp.status().is_success() {
            return Err(error_for_status(resp).await);
        }
        resp.json().await.map_err(backend_err)
    }
}

fn parse_search_response(parsed: Value) -> Result<SearchResults> {
    let total = parsed
        .get("@odata.count")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let hits = parsed
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|hit| {
            let score = hit
                .get("@search.score")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32;
            let document = json_to_document(hit);
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

fn sort_in_memory(hits: &mut [Hit], sorts: &[Sort]) {
    if sorts.is_empty() {
        return;
    }
    hits.sort_by(|a, b| {
        for sort in sorts {
            let ordering = match sort {
                Sort::Score => b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal),
                Sort::Field { name, order } => {
                    let field_ordering = compare_field(a, b, name);
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

fn compare_field(a: &Hit, b: &Hit, name: &str) -> Ordering {
    match (a.document.get(name), b.document.get(name)) {
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    #[tokio::test]
    async fn create_index_sends_field_definitions_and_registers_locally() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/indexes/articles"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "name": "articles" })))
            .mount(&server)
            .await;

        let backend = AzureSearchBackend::new(server.uri(), "test-key");
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
            .and(path("/indexes/articles"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "name": "articles" })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AzureSearchBackend::new(server.uri(), "test-key");
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
    async fn create_index_maps_a_conflict_response_to_already_exists() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/indexes/articles"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": { "code": "OperationNotAllowed", "message": "index articles already exists" }
            })))
            .mount(&server)
            .await;

        let backend = AzureSearchBackend::new(server.uri(), "test-key");
        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        let server = MockServer::start().await;
        let backend = AzureSearchBackend::new(server.uri(), "test-key");

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

    async fn backend_with_articles_index(server: &MockServer) -> AzureSearchBackend {
        Mock::given(method("PUT"))
            .and(path("/indexes/articles"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "name": "articles" })))
            .mount(server)
            .await;

        let backend = AzureSearchBackend::new(server.uri(), "test-key");
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_sends_upload_actions() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/index"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{ "key": "1", "status": true, "statusCode": 200 }]
            })))
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
        let req = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/docs/index")
            .expect("index request was sent");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["value"][0]["@search.action"], "upload");
        assert_eq!(body["value"][0]["id"], "1");
        assert_eq!(body["value"][0]["title"], "Rust async search");
    }

    #[tokio::test]
    async fn index_batch_surfaces_per_document_errors() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/index"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{ "key": "1", "status": false, "statusCode": 400, "errorMessage": "bad field" }]
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
    async fn delete_sends_a_delete_action() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/index"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "value": [{ "key": "1", "status": true, "statusCode": 200 }]
            })))
            .mount(&server)
            .await;

        backend.delete("articles", "1").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let req = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/docs/index")
            .expect("delete request was sent");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["value"][0]["@search.action"], "delete");
        assert_eq!(body["value"][0]["id"], "1");
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
            .and(path("/indexes/articles/docs/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "@odata.count": 1,
                "value": [
                    { "id": "1", "title": "Rust async search", "status": "published", "@search.score": 1.5 }
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
        assert_eq!(results.hits[0].score, 1.5);
        assert_eq!(
            results.hits[0].document.get("title").unwrap(),
            "Rust async search"
        );

        let requests = server.received_requests().await.unwrap();
        let req = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/docs/search")
            .expect("search request was sent");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["search"], "* AND (status:\"published\")");
        assert_eq!(body["queryType"], "full");
    }

    #[tokio::test]
    async fn search_maps_a_404_to_index_not_found() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/search"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "articles"));
    }

    #[tokio::test]
    async fn requests_include_the_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/indexes/articles"))
            .and(header("api-key", "test-key"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "name": "articles" })))
            .mount(&server)
            .await;

        let backend = AzureSearchBackend::new(server.uri(), "test-key");
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn native_sort_uses_orderby_for_a_fast_field() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "@odata.count": 0,
                "value": []
            })))
            .mount(&server)
            .await;

        backend
            .search(
                "articles",
                SearchRequest::new(Query::match_all()).sort(Sort::field("views", SortOrder::Desc)),
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let req = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/docs/search")
            .expect("search request was sent");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["orderby"], "views desc");
        assert_eq!(body["top"], 10);
    }

    #[tokio::test]
    async fn fallback_sort_orders_results_by_a_non_fast_field() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/docs/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "@odata.count": 3,
                "value": [
                    { "id": "1", "status": "b" },
                    { "id": "2", "status": "a" },
                    { "id": "3", "status": "c" }
                ]
            })))
            .mount(&server)
            .await;

        let results = backend
            .search(
                "articles",
                SearchRequest::new(Query::match_all()).sort(Sort::field("status", SortOrder::Asc)),
            )
            .await
            .unwrap();

        let ids: Vec<_> = results.hits.iter().map(|h| h.id.clone()).collect();
        assert_eq!(ids, vec!["2", "1", "3"]);
        assert_eq!(results.total, 3);

        let requests = server.received_requests().await.unwrap();
        let req = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/docs/search")
            .expect("search request was sent");
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body.get("orderby").is_none());
        assert_eq!(body["top"], FALLBACK_SORT_CAP);
    }
}
