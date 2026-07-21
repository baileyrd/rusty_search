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
| [`rusty-search-elasticsearch`](crates/rusty-search-elasticsearch) | A `SearchBackend` that talks to a remote Elasticsearch cluster over HTTP. The only in-process-free backend here besides `rusty-search-opensearch` - see its module docs for what that changes. |
| [`rusty-search-opensearch`](crates/rusty-search-opensearch) | A `SearchBackend` for a remote [OpenSearch](https://opensearch.org) cluster - a thin wrapper around `ElasticsearchBackend`, since OpenSearch still speaks Elasticsearch's wire protocol for everything this crate needs. See ADR-0004 for why it's a wrapper rather than a reimplementation. |
| [`rusty-search-meilisearch`](crates/rusty-search-meilisearch) | A `SearchBackend` for a remote [Meilisearch](https://www.meilisearch.com) instance, via the official `meilisearch-sdk` crate. See its module docs for how its filter-expression query language and async task model shape what's representable. |
| [`rusty-search-solr`](crates/rusty-search-solr) | A `SearchBackend` for a remote [Apache Solr](https://solr.apache.org) instance. Its classic Lucene query syntax can represent the entire `Query` DSL in one string - more than `rusty-search-meilisearch` can - while its own separate `fq` filter mechanism gives it the same genuinely non-scoring filters as Elasticsearch. |
| [`rusty-search-algolia`](crates/rusty-search-algolia) | A `SearchBackend` for the hosted [Algolia](https://www.algolia.com) search SaaS. Like `rusty-search-meilisearch`, at most one `Query::Match` per query; its `filters` expression language nests arbitrarily like Solr's Lucene syntax, but - unlike Solr - has no "match everything" literal, so `must_not` wrapping a bare `Query::MatchAll`/`Query::Match` is rejected the same way Meilisearch rejects it. |
| [`rusty-search`](crates/rusty-search) | The facade crate applications depend on. Re-exports `rusty-search-core` plus each backend behind a feature flag (`memory`, `tantivy`, `elasticsearch`, `opensearch`, `meilisearch`, `solr`, `algolia`), mirroring how `sqlx` gates its database drivers. |

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
the concrete engine at runtime - in-memory in tests, Tantivy, Elasticsearch,
OpenSearch, Meilisearch, Solr, or a hosted Algolia application in production
- exactly as you'd swap a SQLAlchemy engine's connection string. See
[`crates/rusty-search/examples/pluggable_backends.rs`](crates/rusty-search/examples/pluggable_backends.rs)
for a runnable demo that indexes and searches the *same* documents through
each backend with identical calling code:

```sh
cargo run -p rusty-search --example pluggable_backends --features memory,tantivy
# add `,elasticsearch`/`,opensearch`/`,meilisearch`/`,solr`/`,algolia` and set
# RUSTY_SEARCH_ES_URL/RUSTY_SEARCH_OS_URL/RUSTY_SEARCH_MEILI_URL/RUSTY_SEARCH_SOLR_URL/
# RUSTY_SEARCH_ALGOLIA_APP_ID+RUSTY_SEARCH_ALGOLIA_API_KEY
# to also run it against a real cluster/application
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
in-memory map, an Elasticsearch query DSL body, a Meilisearch filter
expression string, a Solr Lucene query string plus `fq` filters, an
Algolia `filters` expression string, ...) - not every backend can
represent every `Query` tree equally well. `rusty-search-meilisearch` and
`rusty-search-algolia` share the sharpest restriction: at most one
`Query::Match` per query, since both search APIs have exactly one
free-text query string. `rusty-search-solr`, by contrast, can represent
the entire `Query` DSL losslessly, since Lucene's query syntax supports
arbitrary boolean nesting in one string *and* has a "match everything"
literal (`*:*`) to ground a lone negative clause against -
`rusty-search-algolia`'s `filters` language nests just as arbitrarily but
lacks that literal, so it rejects `must_not` wrapping a bare
`Query::MatchAll`/`Query::Match` the same way Meilisearch does.

## Adding a new backend

Implement `SearchBackend` for your own type and it plugs into any
application written against the trait - no changes to `rusty-search-core`
or to callers required. `rusty-search-elasticsearch`, `rusty-search-solr`,
`rusty-search-meilisearch`, and `rusty-search-algolia` are reference
examples of independent remote/HTTP backends - three hand-rolled over
`reqwest`, one built on an official SDK - and `rusty-search-opensearch` is
the example of the other legitimate shape a backend can take: a thin
wrapper reusing another backend's translation logic wholesale, when the
underlying wire protocol really is shared.

## Status

This crate is a young, from-scratch project. The core interface and seven
backends (in-memory, Tantivy, Elasticsearch, OpenSearch, Meilisearch,
Solr, Algolia) are implemented and tested; see each crate's module-level
docs for known limitations (e.g. `rusty-search-tantivy`'s sort support,
`rusty-search-meilisearch`'s query restrictions, `rusty-search-algolia`'s
lack of a native per-query field sort or relevance score). Contributions
adding backends for other engines are welcome.

## Project docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — boundaries, structure, data flow, non-goals.
- [docs/adr/](docs/adr/) — the record of individual architectural decisions and their tradeoffs.
- [CONTRIBUTING.md](CONTRIBUTING.md) — workflow, code style, and review expectations.
- [CHANGELOG.md](CHANGELOG.md) / [RELEASE_NOTES.md](RELEASE_NOTES.md) — what shipped, and why; the changelog is the terse list, the release notes carry the reasoning.
- [SECURITY.md](SECURITY.md) — how to report a vulnerability.
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — expectations for participation.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your
option.
