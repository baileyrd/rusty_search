//! A [`SearchBackend`] implementation backed by a remote
//! [Meilisearch](https://www.meilisearch.com) instance, via the official
//! [`meilisearch-sdk`](https://docs.rs/meilisearch-sdk) crate (its default
//! transport is `reqwest` with `rustls`, matching this workspace's other
//! HTTP-based backend, `rusty-search-elasticsearch`).
//!
//! Unlike Elasticsearch, Meilisearch's write operations (index creation,
//! settings updates, document indexing/deletion) are all asynchronous:
//! each one returns a task that's processed in the background. This
//! backend waits for that task to finish before returning from the
//! corresponding `SearchBackend` method, so callers still see
//! synchronous-looking, read-your-writes behavior. Because of that,
//! [`SearchBackend::commit`] is a no-op here - by the time `index`/
//! `index_batch`/`delete` return, the change has already been waited on.
//!
//! ## Known limitations
//!
//! - Meilisearch's search API has exactly one free-text query string per
//!   request. A `Query` tree may contain at most one `Query::Match` clause
//!   (mapped to Meilisearch's `q` + `attributesToSearchOn`); a second one
//!   anywhere in the tree is rejected with [`SearchError::InvalidQuery`].
//!   Every other clause (`Query::Term`, `Query::Range`, and `Query::Bool`'s
//!   boolean structure) compiles down to a Meilisearch filter expression
//!   instead. See [`query_map`] for exactly what is and isn't
//!   representable (e.g. `must_not` wrapping a bare `Query::MatchAll`/
//!   `Query::Match` isn't).
//! - `Query::Range` is only supported on `I64`/`F64` fields - Meilisearch
//!   filter comparison operators (`<`, `>`, `<=`, `>=`) only work on
//!   numbers, not strings (including date strings).
//! - `SearchResults::total` is Meilisearch's `estimatedTotalHits`, which is
//!   exact for small result sets but an estimate for large ones - not a
//!   guaranteed exact count the way the other backends' `total` is.
//! - Like `rusty-search-elasticsearch`, `index_exists`/index-not-found
//!   errors reflect this backend's own local registry of indices it
//!   created, not the cluster's actual state.
//! - `Sort::Score` entries are dropped rather than translated: Meilisearch
//!   has no explicit "sort by relevance" primitive to interleave with field
//!   sorts - relevance is already the default ranking when no `sort` is
//!   given.

mod convert;
pub mod query_map;
mod schema_map;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use meilisearch_sdk::client::Client;
use meilisearch_sdk::errors::{Error as MeiliError, ErrorCode, MeilisearchError};
use meilisearch_sdk::task_info::TaskInfo;
use meilisearch_sdk::tasks::Task;
use serde_json::Value;
use tokio::sync::RwLock;

use rusty_search_core::{
    Document, Hit, Result, Schema as CoreSchema, SearchBackend, SearchError, SearchRequest,
    SearchResults, Sort, SortOrder,
};

use convert::{document_to_json, json_to_document, PRIMARY_KEY};
use schema_map::{build_settings_and_fields, FieldMap};

/// A Meilisearch-backed [`SearchBackend`]. Cheaply cloneable - clones share
/// the same HTTP client and index registry.
#[derive(Clone)]
pub struct MeilisearchBackend {
    client: Client,
    indices: Arc<RwLock<HashMap<String, FieldMap>>>,
}

impl MeilisearchBackend {
    /// Connects to an unauthenticated instance (e.g. a local dev instance
    /// with no master key configured) at `base_url` (e.g.
    /// `"http://localhost:7700"`).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::with_api_key(base_url, None::<String>)
    }

    /// Connects using a Meilisearch API key (master key or a scoped key).
    pub fn with_api_key(
        base_url: impl Into<String>,
        api_key: Option<impl Into<String>>,
    ) -> Result<Self> {
        let client = Client::new(base_url, api_key)
            .map_err(|e| SearchError::Backend(anyhow!(e.to_string())))?;
        Ok(Self {
            client,
            indices: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    async fn require_known(&self, index: &str) -> Result<FieldMap> {
        self.indices
            .read()
            .await
            .get(index)
            .cloned()
            .ok_or_else(|| SearchError::IndexNotFound(index.to_string()))
    }

    /// Waits for a Meilisearch task to finish, mapping a failed task's
    /// error onto [`SearchError`] the same way an immediate HTTP error
    /// would be.
    async fn wait(&self, task_info: TaskInfo, index: &str) -> Result<()> {
        let task = task_info
            .wait_for_completion(&self.client, None, None)
            .await
            .map_err(|e| classify_error(e, index))?;
        match task {
            Task::Failed { content } => Err(classify_meili_error(&content.error, index)),
            _ => Ok(()),
        }
    }
}

fn classify_meili_error(e: &MeilisearchError, index: &str) -> SearchError {
    match e.error_code {
        ErrorCode::IndexAlreadyExists => SearchError::IndexAlreadyExists(index.to_string()),
        ErrorCode::IndexNotFound => SearchError::IndexNotFound(index.to_string()),
        _ => SearchError::Backend(anyhow!("meilisearch error: {e}")),
    }
}

fn classify_error(e: MeiliError, index: &str) -> SearchError {
    match e {
        MeiliError::Meilisearch(me) => classify_meili_error(&me, index),
        other => SearchError::Backend(anyhow!("meilisearch request failed: {other}")),
    }
}

#[async_trait]
impl SearchBackend for MeilisearchBackend {
    async fn create_index(&self, name: &str, schema: CoreSchema) -> Result<()> {
        if self.indices.read().await.contains_key(name) {
            return Err(SearchError::IndexAlreadyExists(name.to_string()));
        }

        let (settings, fields) = build_settings_and_fields(&schema);

        let task_info = self
            .client
            .create_index(name, Some(PRIMARY_KEY))
            .await
            .map_err(|e| classify_error(e, name))?;
        self.wait(task_info, name).await?;

        let index = self.client.index(name);
        let task_info = index
            .set_settings(&settings)
            .await
            .map_err(|e| classify_error(e, name))?;
        self.wait(task_info, name).await?;

        self.indices.write().await.insert(name.to_string(), fields);
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> Result<()> {
        self.require_known(name).await?;

        let task_info = self
            .client
            .delete_index(name)
            .await
            .map_err(|e| classify_error(e, name))?;
        self.wait(task_info, name).await?;

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

        let json_docs: Vec<Value> = documents
            .into_iter()
            .map(|doc| document_to_json(doc).1)
            .collect();

        let idx = self.client.index(index);
        let task_info = idx
            .add_documents(&json_docs, Some(PRIMARY_KEY))
            .await
            .map_err(|e| classify_error(e, index))?;
        self.wait(task_info, index).await
    }

    async fn delete(&self, index: &str, id: &str) -> Result<()> {
        self.require_known(index).await?;

        let idx = self.client.index(index);
        let task_info = idx
            .delete_document(id)
            .await
            .map_err(|e| classify_error(e, index))?;
        // Deleting a nonexistent document is itself a successful (no-op)
        // Meilisearch task, so no special not-found handling is needed here.
        self.wait(task_info, index).await
    }

    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults> {
        let fields = self.require_known(index).await?;
        let params = query_map::build_search_params(&request.query, &fields)?;

        let idx = self.client.index(index);
        let mut search_query = idx.search();
        search_query.with_offset(request.offset);
        search_query.with_limit(request.limit);
        search_query.with_show_ranking_score(true);

        let full_text_value = params.full_text.as_ref().map_or("", |(_, v)| v.as_str());
        search_query.with_query(full_text_value);

        let search_fields: Vec<&str> = params
            .full_text
            .as_ref()
            .map(|(f, _)| vec![f.as_str()])
            .unwrap_or_default();
        if !search_fields.is_empty() {
            search_query.with_attributes_to_search_on(&search_fields);
        }

        if let Some(filter) = params.filter.as_deref() {
            search_query.with_filter(filter);
        }

        let sort_strings: Vec<String> = request.sort.iter().filter_map(sort_to_meili).collect();
        let sort_refs: Vec<&str> = sort_strings.iter().map(String::as_str).collect();
        if !sort_refs.is_empty() {
            search_query.with_sort(&sort_refs);
        }

        let results: meilisearch_sdk::search::SearchResults<Value> =
            search_query
                .execute()
                .await
                .map_err(|e| classify_error(e, index))?;

        let total = results.estimated_total_hits.unwrap_or(0);
        let hits = results
            .hits
            .into_iter()
            .map(|hit| {
                let score = hit.ranking_score.unwrap_or(0.0) as f32;
                let document = json_to_document(hit.result);
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
        // Every write above already waits for its Meilisearch task to
        // finish, so there is nothing left to flush by the time `commit`
        // is called.
        self.require_known(index).await?;
        Ok(())
    }
}

fn sort_to_meili(sort: &Sort) -> Option<String> {
    match sort {
        Sort::Score => None,
        Sort::Field { name, order } => {
            let order_str = match order {
                SortOrder::Asc => "asc",
                SortOrder::Desc => "desc",
            };
            Some(format!("{name}:{order_str}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_search_core::{FieldOptions, Query, Schema};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn task_info_json(uid: u32, index_uid: &str) -> Value {
        json!({
            "taskUid": uid,
            "indexUid": index_uid,
            "status": "enqueued",
            "type": "indexCreation",
            "enqueuedAt": "2024-01-01T00:00:00Z",
        })
    }

    fn succeeded_task_json(uid: u32, index_uid: &str) -> Value {
        json!({
            "status": "succeeded",
            "uid": uid,
            "indexUid": index_uid,
            "type": "indexCreation",
            "duration": "PT0.1S",
            "enqueuedAt": "2024-01-01T00:00:00Z",
            "startedAt": "2024-01-01T00:00:00Z",
            "finishedAt": "2024-01-01T00:00:00.100Z",
            "canceledBy": null,
            "error": null,
        })
    }

    fn failed_task_json(uid: u32, index_uid: &str, error_code: &str) -> Value {
        json!({
            "status": "failed",
            "uid": uid,
            "indexUid": index_uid,
            "type": "indexCreation",
            "duration": "PT0.1S",
            "enqueuedAt": "2024-01-01T00:00:00Z",
            "startedAt": "2024-01-01T00:00:00Z",
            "finishedAt": "2024-01-01T00:00:00.100Z",
            "canceledBy": null,
            "error": {
                "message": "failure",
                "code": error_code,
                "type": "invalid_request",
                "link": "https://docs.meilisearch.com/errors",
            },
        })
    }

    /// Mounts a `GET /tasks/0` mock that always reports task 0 as
    /// succeeded (or failed with `error_code`, if given) - every write
    /// operation in these tests enqueues as task uid 0, since none of our
    /// glue code inspects the task's `type`/`details`.
    async fn mock_task_completion(server: &MockServer, index_uid: &str, error_code: Option<&str>) {
        let body = match error_code {
            Some(code) => failed_task_json(0, index_uid, code),
            None => succeeded_task_json(0, index_uid),
        };
        Mock::given(method("GET"))
            .and(path("/tasks/0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn articles_schema() -> Schema {
        Schema::builder()
            .text("title")
            .keyword("status")
            .i64_field_with("views", FieldOptions::new().fast(true))
            .build()
    }

    #[tokio::test]
    async fn create_index_sends_settings_and_registers_locally() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/indexes"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(&server)
            .await;
        mock_task_completion(&server, "articles", None).await;

        let backend = MeilisearchBackend::new(server.uri()).unwrap();
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();

        assert!(backend.index_exists("articles").await.unwrap());
    }

    #[tokio::test]
    async fn create_index_rejects_duplicates_without_a_second_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/indexes"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(&server)
            .await;
        mock_task_completion(&server, "articles", None).await;

        let backend = MeilisearchBackend::new(server.uri()).unwrap();
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
    async fn create_index_maps_a_failed_task_conflict_to_already_exists() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/indexes"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(&server)
            .await;
        mock_task_completion(&server, "articles", Some("index_already_exists")).await;

        let backend = MeilisearchBackend::new(server.uri()).unwrap();
        let err = backend
            .create_index("articles", articles_schema())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexAlreadyExists(name) if name == "articles"));
    }

    #[tokio::test]
    async fn operations_on_unknown_index_error_without_any_http_call() {
        let server = MockServer::start().await;
        let backend = MeilisearchBackend::new(server.uri()).unwrap();

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

    async fn backend_with_articles_index(server: &MockServer) -> MeilisearchBackend {
        Mock::given(method("POST"))
            .and(path("/indexes"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/indexes/articles/settings"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(server)
            .await;
        mock_task_completion(server, "articles", None).await;

        let backend = MeilisearchBackend::new(server.uri()).unwrap();
        backend
            .create_index("articles", articles_schema())
            .await
            .unwrap();
        backend
    }

    #[tokio::test]
    async fn index_batch_sends_documents_and_waits_for_the_task() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/documents"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
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
        let add_request = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/documents")
            .expect("add-documents request was sent");
        let body: Value = serde_json::from_slice(&add_request.body).unwrap();
        assert_eq!(body[0]["id"], "1");
        assert_eq!(body[0]["title"], "Rust async search");
    }

    #[tokio::test]
    async fn delete_and_commit_round_trip() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("DELETE"))
            .and(path("/indexes/articles/documents/1"))
            .respond_with(ResponseTemplate::new(202).set_body_json(task_info_json(0, "articles")))
            .mount(&server)
            .await;

        backend.delete("articles", "1").await.unwrap();
        backend.commit("articles").await.unwrap();
    }

    #[tokio::test]
    async fn search_sends_translated_filter_and_parses_hits() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": [
                    { "id": "1", "title": "Rust async search", "status": "published", "_rankingScore": 0.95 }
                ],
                "estimatedTotalHits": 1,
                "processingTimeMs": 1,
                "query": "",
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
        assert_eq!(results.hits[0].score, 0.95);
        assert_eq!(
            results.hits[0].document.get("title").unwrap(),
            "Rust async search"
        );

        let requests = server.received_requests().await.unwrap();
        let search_request = requests
            .iter()
            .find(|r| r.url.path() == "/indexes/articles/search")
            .expect("search request was sent");
        let body: Value = serde_json::from_slice(&search_request.body).unwrap();
        assert_eq!(body["filter"], "status = \"published\"");
    }

    #[tokio::test]
    async fn search_maps_an_immediate_404_to_index_not_found() {
        let server = MockServer::start().await;
        let backend = backend_with_articles_index(&server).await;

        Mock::given(method("POST"))
            .and(path("/indexes/articles/search"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "message": "Index `articles` not found.",
                "code": "index_not_found",
                "type": "invalid_request",
                "link": "https://docs.meilisearch.com/errors",
            })))
            .mount(&server)
            .await;

        let err = backend
            .search("articles", Query::match_all().into())
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::IndexNotFound(name) if name == "articles"));
    }
}
