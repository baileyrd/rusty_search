//! Demonstrates the whole point of `rusty_search`: application code is
//! written once against `Arc<dyn SearchBackend>`, and the concrete engine
//! underneath - an in-memory index here, an embedded Tantivy index there -
//! is swapped without changing a single line of `run_demo`.
//!
//! Run with:
//!   cargo run -p rusty-search --example pluggable_backends --features memory,tantivy

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

    Ok(())
}
