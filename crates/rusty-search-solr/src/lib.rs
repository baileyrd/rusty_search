//! A [`SearchBackend`] implementation backed by a remote
//! [Apache Solr](https://solr.apache.org) instance, talked to over HTTP
//! via [`reqwest`].
//!
//! Like `rusty-search-elasticsearch`, this backend has no in-process index
//! of its own and keeps a small local registry (which indices it created,
//! their field types) instead of round-tripping to the server for that on
//! every call - see that crate's docs for the same tradeoff spelled out in
//! more detail.
//!
//! Solr's classic query parser is more expressive in one string than
//! Meilisearch's split `q`/filter-expression model: it supports arbitrary
//! boolean nesting (`AND`/`OR`/`NOT`, parentheses) directly. Because of
//! that, `rusty-search-solr` can represent an entire `Query` tree -
//! including more than one `Query::Match` and `must_not` wrapping a bare
//! `Query::MatchAll`/`Query::Match` - that `rusty-search-meilisearch`
//! has to reject. See [`query_map`] for the translation.
//!
//! ## Known limitations
//!
//! - `create_index` creates a Solr *core* via the Core Admin API using the
//!   `_default` configset (which ships with modern standalone Solr
//!   distributions and uses a managed schema). It does not use the
//!   Collections API, so it won't work against a SolrCloud cluster in
//!   cloud mode, and it assumes `_default` is available - a Solr install
//!   without that configset will fail index creation.
//! - `Query::Match` compiles to a quoted phrase query (`field:"value"`),
//!   analyzed through the field's tokenizer - not an OR-of-terms match the
//!   way Elasticsearch's `match` defaults to. A caller relying on
//!   any-term-matches semantics across backends should account for this
//!   difference.
//! - `Query::Range` is supported on `I64`/`F64`/`Date` fields (Solr's
//!   Lucene range syntax handles all three), but not `Keyword`/`Text`/
//!   `Bool`.
//! - Like `rusty-search-elasticsearch`, `index_exists`/index-not-found
//!   errors reflect this backend's own local registry of indices it
//!   created, not the server's actual state.

mod convert;
pub mod query_map;
mod schema_map;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, Hit, Result, Schema as CoreSchema, SearchBackend, SearchError, SearchRequest,
    SearchResults,
};

use convert::{document_to_json, json_to_document};
use schema_map::{build_add_field_body, FieldMap};

#[derive(Clone)]
enum Auth {
    None,
    Basic { username: String, password: String },
}

/// An Apache Solr-backed [`SearchBackend`]. Cheaply cloneable - clones
/// share the same HTTP client and index registry.
#[derive(Clone)]
pub struct SolrBackend {
    client: reqwest::Client,
    base_url: String,
    auth: Auth,
    indices: Arc<RwLock<HashMap<String, FieldMap>>>,
}

impl SolrBackend {
    /// Connects to an unauthenticated instance at `base_url` (e.g.
    /// `"http://localhost:8983"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client_and_auth(base_url, reqwest::Client::new(), Auth::None)
    }

    /// Connects using HTTP basic auth (Solr's Basic Authentication
    /// Plugin).
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

    /// Connects with a caller-supplied [`reqwest::Client`] (for custom
    /// timeouts, TLS config, proxies, etc).
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

    /// Reads a response, defensively handling Solr's inconsistent HTTP
    /// status passthrough: some deployments embed a non-2xx failure as an
    /// `"error"` object in a `200 OK` body rather than returning the real
    /// status code, so this checks for that shape first regardless of the
    /// HTTP status, and only falls back to the status code otherwise.
    async fn parse_response(&self, resp: reqwest::Response, index: &str) -> Result<Value> {
        let status = resp.status();
        let text = resp.text().await.map_err(backend_err)?;
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

        if let Some(error) = json.get("error") {
            let msg = error.get("msg").and_then(Value::as_str).unwrap_or(&text);
            let lower = msg.to_lowercase();
            if lower.contains("already exists") {
                return Err(SearchError::IndexAlreadyExists(index.to_string()));
            }
            if lower.contains("not found")
                || lower.contains("no such core")
                || lower.contains("no core")
            {
                return Err(SearchError::IndexNotFound(index.to_string()));
            }
            return Err(SearchError::Backend(anyhow!("solr error: {msg}")));
        }

        if status == StatusCode::NOT_FOUND {
            return Err(SearchError::IndexNotFound(index.to_string()));
        }
        if !status.is_success() {
            return Err(SearchError::Backend(anyhow!(
                "solr returned {status}: {text}"
            )));
        }

        Ok(json)
    }
}

fn backend_err(e: impl std::error::Error + Send + Sync + 'static) -> SearchError {
    SearchError::Backend(anyhow::Error::new(e))
}

#[async_trait]
impl SearchBackend for SolrBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        if self.indices.read().await.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let resp = self
            .request(Method::POST, "solr/admin/cores")
            .query(&[
                ("action", "CREATE"),
                ("name", name),
                ("configSet", "_default"),
                ("wt", "json"),
            ])
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, name).await?;

        let (body, fields) = build_add_field_body(&schema);
        let resp = self
            .request(Method::POST, &format!("solr/{name}/schema"))
            .json(&body)
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, name).await?;

        self.indices.write().await.insert(name.to_string(), fields);
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        self.require_known(name).await?;

        let resp = self
            .request(Method::POST, "solr/admin/cores")
            .query(&[
                ("action", "UNLOAD"),
                ("core", name),
                ("deleteInstanceDir", "true"),
                ("wt", "json"),
            ])
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, name).await?;

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

        let commands: Vec<Value> = documents
            .into_iter()
            .map(|doc| {
                let (_, json) = document_to_json(doc);
                json!({ "add": { "doc": json } })
            })
            .collect();

        let resp = self
            .request(Method::POST, &format!("solr/{index}/update"))
            .query(&[("wt", "json")])
            .json(&commands)
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, index).await?;
        Ok(())
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.require_known(index).await?;

        let resp = self
            .request(Method::POST, &format!("solr/{index}/update"))
            .query(&[("wt", "json")])
            .json(&json!({ "delete": { "id": id } }))
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, index).await?;
        Ok(())
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let fields = self.require_known(index).await?;
        let params = query_map::build_search_params(&request.query, &fields)?;

        let mut query_params: Vec<(&str, String)> = vec![
            ("q", params.q),
            ("start", request.offset.to_string()),
            ("rows", request.limit.to_string()),
            ("wt", "json".to_string()),
            ("fl", "*,score".to_string()),
        ];
        for fq in &params.fq {
            query_params.push(("fq", fq.clone()));
        }
        if let Some(sort) = query_map::sort_to_solr(&request.sort) {
            query_params.push(("sort", sort));
        }

        let resp = self
            .request(Method::GET, &format!("solr/{index}/select"))
            .query(&query_params)
            .send()
            .await
            .map_err(backend_err)?;
        let json = self.parse_response(resp, index).await?;

        parse_search_response(json)
    }

    async fn commit(&self, index: &str) -> Result<()> {
        self.require_known(index).await?;

        let resp = self
            .request(Method::POST, &format!("solr/{index}/update"))
            .query(&[("wt", "json")])
            .json(&json!({ "commit": {} }))
            .send()
            .await
            .map_err(backend_err)?;
        self.parse_response(resp, index).await?;
        Ok(())
    }
}

fn parse_search_response(json: Value) -> Result<SearchResults> {
    let response = json.get("response").ok_or_else(|| {
        SearchError::Backend(anyhow!("malformed solr response: missing `response`"))
    })?;

    let total = response
        .get("numFound")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let hits = response
        .get("docs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|doc| {
            let score = doc.get("score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let document = json_to_document(doc);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Query, Schema};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    fn ok_body() -> Value {
        json!({ "responseHeader": { "status": 0, "QTime": 1 } })
    }

    #[tokio::test]
    async fn create_index_creates_core_and_schema_then_registers_locally() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/solr/admin/cores"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/solr/articles/schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;

        let backend = SolrBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        assert!(backend.index_exists("articles").await.unwrap());

        let requests = server.received_requests().await.unwrap();
        let create = requests
            .iter()
            .find(|r| r.url.path() == "/solr/admin/cores")
            .expect("core create request was sent");
        let query: std::collections::HashMap<_, _> = create.url.query_pairs().collect();
        assert_eq!(query.get("action").map(|v| v.as_ref()), Some("CREATE"));
        assert_eq!(query.get("name").map(|v| v.as_ref()), Some("articles"));
    }

    #[tokio::test]
    async fn create_index_rejects_duplicates_without_a_second_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/solr/admin/cores"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/solr/articles/schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;

        let backend = SolrBackend::new(server.uri());
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
    async fn create_index_maps_an_embedded_error_object_to_already_exists() {
        // Solr sometimes embeds a failure as an "error" object in an
        // HTTP 200 body rather than a real non-2xx status.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/solr/admin/cores"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responseHeader": { "status": 400 },
                "error": { "msg": "Core with name 'articles' already exists.", "code": 400 }
            })))
            .mount(&server)
            .await;

        let backend = SolrBackend::new(server.uri());
        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        let server = MockServer::start().await;
        let backend = SolrBackend::new(server.uri());

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

    async fn backend_with_articles_index(server: &MockServer) -> SolrBackend {
        Mock::given(method("POST"))
            .and(path("/solr/admin/cores"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/solr/articles/schema"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(server)
            .await;

        let backend = SolrBackend::new(server.uri());
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_sends_add_commands() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/solr/articles/update"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
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
        let update = requests
            .iter()
            .find(|r| r.url.path() == "/solr/articles/update")
            .expect("update request was sent");
        let body: Value = serde_json::from_slice(&update.body).unwrap();
        assert_eq!(body[0]["add"]["doc"]["id"], "1");
        assert_eq!(body[0]["add"]["doc"]["title"], "Rust async search");
    }

    #[tokio::test]
    async fn delete_sends_a_delete_command() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/solr/articles/update"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;

        backend.delete("articles", "1").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let update = requests
            .iter()
            .find(|r| r.url.path() == "/solr/articles/update")
            .expect("update request was sent");
        let body: Value = serde_json::from_slice(&update.body).unwrap();
        assert_eq!(body["delete"]["id"], "1");
    }

    #[tokio::test]
    async fn commit_sends_a_commit_command() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/solr/articles/update"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;

        backend.commit("articles").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let update = requests
            .iter()
            .find(|r| r.url.path() == "/solr/articles/update")
            .expect("update request was sent");
        let body: Value = serde_json::from_slice(&update.body).unwrap();
        assert!(body.get("commit").is_some());
    }

    #[tokio::test]
    async fn search_sends_translated_query_and_parses_hits() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("GET"))
            .and(path("/solr/articles/select"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responseHeader": { "status": 0, "QTime": 1 },
                "response": {
                    "numFound": 1,
                    "start": 0,
                    "docs": [
                        { "id": "1", "title": "Rust async search", "status": "published", "score": 1.5 }
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

        let requests = server.received_requests().await.unwrap();
        let select = requests
            .iter()
            .find(|r| r.url.path() == "/solr/articles/select")
            .expect("select request was sent");
        let query: std::collections::HashMap<_, _> = select.url.query_pairs().collect();
        assert_eq!(
            query.get("q").map(|v| v.as_ref()),
            Some("*:* AND (status:\"published\")")
        );
    }

    #[tokio::test]
    async fn search_maps_an_embedded_not_found_error_to_index_not_found() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("GET"))
            .and(path("/solr/articles/select"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "responseHeader": { "status": 404 },
                "error": { "msg": "no core found for articles", "code": 404 }
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
    async fn with_basic_auth_sends_the_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/solr/admin/cores"))
            .and(wiremock::matchers::basic_auth("solr", "solr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/solr/articles/schema"))
            .and(wiremock::matchers::basic_auth("solr", "solr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;

        let backend = SolrBackend::with_basic_auth(server.uri(), "solr", "solr");
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
    }
}
