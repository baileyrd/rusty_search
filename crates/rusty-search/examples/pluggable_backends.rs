//! Demonstrates the whole point of `rusty_search`: application code is
//! written once against `Arc<dyn SearchBackend>`, and the concrete engine
//! underneath - an in-memory index here, an embedded Tantivy index there, a
//! remote Elasticsearch, OpenSearch, Meilisearch, or Solr cluster, or a
//! hosted Algolia or Azure AI Search application over there - is swapped
//! without changing a single line of `run_demo`.
//!
//! Run with:
//!   cargo run -p rusty-search --example pluggable_backends --features memory,tantivy
//!
//! Add `,elasticsearch`/`,opensearch`/`,meilisearch`/`,solr`/`,algolia`/
//! `,azure-search` to `--features` and set `RUSTY_SEARCH_ES_URL` /
//! `RUSTY_SEARCH_OS_URL` (both e.g. `http://localhost:9200`) /
//! `RUSTY_SEARCH_MEILI_URL` (e.g. `http://localhost:7700`) /
//! `RUSTY_SEARCH_SOLR_URL` (e.g. `http://localhost:8983`) /
//! `RUSTY_SEARCH_ALGOLIA_APP_ID` + `RUSTY_SEARCH_ALGOLIA_API_KEY` /
//! `RUSTY_SEARCH_AZURE_SEARCH_ENDPOINT` (e.g.
//! `https://my-service.search.windows.net`) + `RUSTY_SEARCH_AZURE_SEARCH_API_KEY`
//! to also run the demo against a real cluster/application; without those
//! env vars, those legs are skipped rather than failing, since they need
//! infrastructure (or a hosted account) the other backends don't.

use std::sync::Arc;

use rusty_search::{
    Document, FieldOptions, Query, Schema, SearchBackend, SearchRequest, Sort, SortOrder,
};

async fn run_demo(backend: Arc<dyn SearchBackend>, label: &str) -> rusty_search::Result<()> {
    println!("--- {label} ---");

    let schema = Schema::builder()
        .text("title")
        .keyword("status")
        .i64_field_with("views", FieldOptions::new().fast(true))
        .build();

    // Swapping backends means we might run this demo more than once against
    // a backend that persists across calls, so make index creation
    // idempotent for the sake of the demo.
    if backend.index_exists("articles").await? {
        backend.delete_index("articles").await?;
    }
    backend.create_index("articles", schema).await?;

    backend
        .index_batch(
            "articles",
            vec![
                Document::new()
                    .with_id("1")
                    .set("title", "Rust async search made pluggable")
                    .set("status", "published")
                    .set("views", 120),
                Document::new()
                    .with_id("2")
                    .set("title", "Async Rust patterns")
                    .set("status", "draft")
                    .set("views", 40),
                Document::new()
                    .with_id("3")
                    .set("title", "Cooking with cast iron")
                    .set("status", "published")
                    .set("views", 75),
            ],
        )
        .await?;
    backend.commit("articles").await?;

    let results = backend
        .search(
            "articles",
            SearchRequest::new(
                Query::match_query("title", "async rust").and(Query::term("status", "published")),
            )
            .sort(Sort::field("views", SortOrder::Desc)),
        )
        .await?;

    println!("matched {} document(s):", results.total);
    for hit in &results.hits {
        println!(
            "  #{} score={:.2} title={:?} views={:?}",
            hit.id,
            hit.score,
            hit.document.get("title"),
            hit.document.get("views"),
        );
    }
    println!();

    Ok(())
}

#[tokio::main]
async fn main() -> rusty_search::Result<()> {
    #[cfg(feature = "memory")]
    run_demo(
        Arc::new(rusty_search::MemoryBackend::new()),
        "MemoryBackend",
    )
    .await?;

    #[cfg(feature = "tantivy")]
    run_demo(
        Arc::new(rusty_search::TantivyBackend::in_memory()),
        "TantivyBackend",
    )
    .await?;

    #[cfg(feature = "elasticsearch")]
    match std::env::var("RUSTY_SEARCH_ES_URL") {
        Ok(url) => {
            run_demo(Arc::new(rusty_search::ElasticsearchBackend::new(url)), "ElasticsearchBackend")
                .await?
        }
        Err(_) => println!("--- ElasticsearchBackend --- skipped (set RUSTY_SEARCH_ES_URL to run against a real cluster)\n"),
    }

    #[cfg(feature = "opensearch")]
    match std::env::var("RUSTY_SEARCH_OS_URL") {
        Ok(url) => {
            run_demo(Arc::new(rusty_search::OpenSearchBackend::new(url)), "OpenSearchBackend").await?
        }
        Err(_) => println!("--- OpenSearchBackend --- skipped (set RUSTY_SEARCH_OS_URL to run against a real cluster)\n"),
    }

    #[cfg(feature = "meilisearch")]
    match std::env::var("RUSTY_SEARCH_MEILI_URL") {
        Ok(url) => {
            let backend = rusty_search::MeilisearchBackend::new(url)?;
            run_demo(Arc::new(backend), "MeilisearchBackend").await?
        }
        Err(_) => println!("--- MeilisearchBackend --- skipped (set RUSTY_SEARCH_MEILI_URL to run against a real instance)\n"),
    }

    #[cfg(feature = "solr")]
    match std::env::var("RUSTY_SEARCH_SOLR_URL") {
        Ok(url) => run_demo(Arc::new(rusty_search::SolrBackend::new(url)), "SolrBackend").await?,
        Err(_) => println!("--- SolrBackend --- skipped (set RUSTY_SEARCH_SOLR_URL to run against a real instance)\n"),
    }

    #[cfg(feature = "algolia")]
    match (
        std::env::var("RUSTY_SEARCH_ALGOLIA_APP_ID"),
        std::env::var("RUSTY_SEARCH_ALGOLIA_API_KEY"),
    ) {
        (Ok(app_id), Ok(api_key)) => {
            run_demo(Arc::new(rusty_search::AlgoliaBackend::new(app_id, api_key)), "AlgoliaBackend")
                .await?
        }
        _ => println!(
            "--- AlgoliaBackend --- skipped (set RUSTY_SEARCH_ALGOLIA_APP_ID and RUSTY_SEARCH_ALGOLIA_API_KEY to run against a real application)\n"
        ),
    }

    #[cfg(feature = "azure-search")]
    match (
        std::env::var("RUSTY_SEARCH_AZURE_SEARCH_ENDPOINT"),
        std::env::var("RUSTY_SEARCH_AZURE_SEARCH_API_KEY"),
    ) {
        (Ok(endpoint), Ok(api_key)) => {
            run_demo(
                Arc::new(rusty_search::AzureSearchBackend::new(endpoint, api_key)),
                "AzureSearchBackend",
            )
            .await?
        }
        _ => println!(
            "--- AzureSearchBackend --- skipped (set RUSTY_SEARCH_AZURE_SEARCH_ENDPOINT and RUSTY_SEARCH_AZURE_SEARCH_API_KEY to run against a real service)\n"
        ),
    }

    Ok(())
}
