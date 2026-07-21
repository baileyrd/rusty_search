# rusty_search

An async, pluggable search interface for Rust. Application code is written
once against a standard `SearchBackend` trait; the concrete search engine
underneath - an in-memory index, an embedded [Tantivy](https://github.com/quickwit-oss/tantivy)
index, a remote cluster - is chosen (and swappable) at construction time,
without touching call sites.

This is the same idea SQLAlchemy applies to databases: one `Engine`
interface, many interchangeable dialects underneath. `rusty_search` applies
it to search.

```rust
use std::sync::Arc;
use rusty_search::{Document, Query, Schema, SearchBackend};

#[tokio::main]
async fn main() -> rusty_search::Result<()> {
    // Swap this line for `TantivyBackend::in_memory()` (or your own
    // `SearchBackend` impl) and every line below stays exactly the same.
    let backend: Arc<dyn SearchBackend> = Arc::new(rusty_search::MemoryBackend::new());

    backend.create_index("articles", Schema::builder().text("title").build()).await?;
    backend.index("articles", Document::new().with_id("1").set("title", "Rust async search")).await?;
    backend.commit("articles").await?;

    let results = backend.search("articles", Query::match_query("title", "rust").into()).await?;
    assert_eq!(results.total, 1);
    Ok(())
}
```

(Requires the `memory` feature: `rusty-search = { version = "0.1", features = ["memory"] }`.)

## Workspace layout

| Crate | Purpose |
| --- | --- |
| [`rusty-search-core`](crates/rusty-search-core) | The standard vocabulary: `Document`, `Schema`, `Query`, `SearchRequest`/`SearchResults`, and the `SearchBackend` trait. No concrete engine - depend on this alone to write backend-agnostic application code or your own backend. |
| [`rusty-search-memory`](crates/rusty-search-memory) | A dependency-free, in-memory `SearchBackend`. No external engine, `O(documents)` per search - the search equivalent of pointing SQLAlchemy at SQLite. Ideal for tests. |
| [`rusty-search-tantivy`](crates/rusty-search-tantivy) | A `SearchBackend` backed by [Tantivy](https://github.com/quickwit-oss/tantivy), an embedded, real inverted-index full-text search engine. Runs in-process (`TantivyBackend::in_memory()`) or persists to disk (`TantivyBackend::on_disk(dir)`). |
| [`rusty-search`](crates/rusty-search) | The facade crate applications depend on. Re-exports `rusty-search-core` plus each backend behind a feature flag (`memory`, `tantivy`), mirroring how `sqlx` gates its database drivers. |

## Why a trait, not a struct

`SearchBackend` is written with [`async-trait`](https://docs.rs/async-trait)
specifically so it stays object-safe:

```rust,ignore
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn create_index(&self, name: &str, schema: Schema) -> Result<()>;
    async fn delete_index(&self, name: &str) -> Result<()>;
    async fn index_exists(&self, name: &str) -> Result<bool>;
    async fn index_batch(&self, index: &str, documents: Vec<Document>) -> Result<()>;
    async fn delete(&self, index: &str, id: &str) -> Result<()>;
    async fn search(&self, index: &str, request: SearchRequest) -> Result<SearchResults>;
    async fn commit(&self, index: &str) -> Result<()>;
}
```

That means application code can hold an `Arc<dyn SearchBackend>` and swap
the concrete engine at runtime - in-memory in tests, Tantivy (or your own
Elasticsearch/Meilisearch/OpenSearch client) in production - exactly as
you'd swap a SQLAlchemy engine's connection string. See
[`crates/rusty-search/examples/pluggable_backends.rs`](crates/rusty-search/examples/pluggable_backends.rs)
for a runnable demo that indexes and searches the *same* documents through
both backends with identical calling code:

```sh
cargo run -p rusty-search --example pluggable_backends --features memory,tantivy
```

## The query DSL

Queries are built from a small set of composable primitives - the search
equivalent of SQLAlchemy Core's expression language - rather than a
backend-specific query string:

```rust
use rusty_search::Query;

let query = Query::match_query("body", "async search")
    .and(Query::term("status", "published"))
    .and(Query::range("views", Some(100.into()), None));
```

Every backend translates the same `Query` tree into its own native
representation (a Tantivy `Query`, a hand-rolled evaluator over an
in-memory map, an Elasticsearch query DSL body, ...).

## Adding a new backend

Implement `SearchBackend` for your own type and it plugs into any
application written against the trait - no changes to `rusty-search-core`
or to callers required. A backend for a remote engine (Elasticsearch,
Meilisearch, OpenSearch) is a natural next step; it just needs to translate
`Query`/`Schema` into that engine's HTTP API instead of Tantivy's in-process
one.

## Status

This crate is a young, from-scratch project. The core interface and two
backends (in-memory, Tantivy) are implemented and tested; see each crate's
module-level docs for known limitations (e.g. `rusty-search-tantivy`'s
sort support). Contributions adding backends for other engines are welcome.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
