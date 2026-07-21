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
| [`rusty-search-azure-search`](crates/rusty-search-azure-search) | A `SearchBackend` for the hosted [Azure AI Search](https://azure.microsoft.com/en-us/products/ai-services/ai-search) service. Its full-Lucene-syntax `search` parameter is as expressive as Solr's `q` (more than one `Query::Match`, `must_not` wrapping a bare `Query::Match`), plus a genuinely separate OData `$filter` for `Query::Bool::filter`; sorting mirrors `rusty-search-tantivy`'s fast fields rather than Elasticsearch's everything-sortable default. |
| [`rusty-search`](crates/rusty-search) | The facade crate applications depend on. Re-exports `rusty-search-core` plus each backend behind a feature flag (`memory`, `tantivy`, `elasticsearch`, `opensearch`, `meilisearch`, `solr`, `algolia`, `azure-search`), mirroring how `sqlx` gates its database drivers. |

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
OpenSearch, Meilisearch, Solr, a hosted Algolia application, or a hosted
Azure AI Search service in production - exactly as you'd swap a SQLAlchemy
engine's connection string. See
[`crates/rusty-search/examples/pluggable_backends.rs`](crates/rusty-search/examples/pluggable_backends.rs)
for a runnable demo that indexes and searches the *same* documents through
each backend with identical calling code:

```sh
cargo run -p rusty-search --example pluggable_backends --features memory,tantivy
# add `,elasticsearch`/`,opensearch`/`,meilisearch`/`,solr`/`,algolia`/
# `,azure-search` and set
# RUSTY_SEARCH_ES_URL/RUSTY_SEARCH_OS_URL/RUSTY_SEARCH_MEILI_URL/RUSTY_SEARCH_SOLR_URL/
# RUSTY_SEARCH_ALGOLIA_APP_ID+RUSTY_SEARCH_ALGOLIA_API_KEY/
# RUSTY_SEARCH_AZURE_SEARCH_ENDPOINT+RUSTY_SEARCH_AZURE_SEARCH_API_KEY
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
Algolia `filters` expression string, an Azure AI Search full-Lucene-syntax
`search` string plus an OData `$filter`, ...) - not every backend can
represent every `Query` tree equally well. `rusty-search-meilisearch` and
`rusty-search-algolia` share the sharpest restriction: at most one
`Query::Match` per query, since both search APIs have exactly one
free-text query string. `rusty-search-solr` and `rusty-search-azure-search`,
by contrast, can represent more than one `Query::Match`, since Lucene
syntax (which Azure's `search` parameter also speaks, via `queryType:
"full"`) supports arbitrary boolean nesting and field-scoped clauses in
one string. Solr additionally has a "match everything" literal (`*:*`) to
ground a lone negative clause against anywhere in the tree; Azure only
trusts that grounding at the outermost position, so `must_not` wrapping a
bare `Query::MatchAll` is representable there only inside
`Query::Bool::filter` (via OData's real `true`/`false` literals), not in
`must`/`should`/`must_not` - see `rusty-search-azure-search`'s module docs
for the exact boundary.

## Adding a new backend

Implement `SearchBackend` for your own type and it plugs into any
application written against the trait - no changes to `rusty-search-core`
or to callers required. `rusty-search-elasticsearch`, `rusty-search-solr`,
`rusty-search-meilisearch`, `rusty-search-algolia`, and
`rusty-search-azure-search` are reference examples of independent
remote/HTTP backends - four hand-rolled over `reqwest`, one built on an
official SDK - and `rusty-search-opensearch` is the example of the other
legitimate shape a backend can take: a thin wrapper reusing another
backend's translation logic wholesale, when the underlying wire protocol
really is shared.

## Status

This crate is a young, from-scratch project. The core interface and eight
backends (in-memory, Tantivy, Elasticsearch, OpenSearch, Meilisearch,
Solr, Algolia, Azure AI Search) are implemented and tested; see each
crate's module-level docs for known limitations (e.g.
`rusty-search-tantivy`'s sort support, `rusty-search-meilisearch`'s query
restrictions, `rusty-search-algolia`'s lack of a native per-query field
sort or relevance score, `rusty-search-azure-search`'s fast-field-like
sortable requirement). Contributions adding backends for other engines are
welcome.

### Planned backends

No firm commitments or timelines - candidates being considered, roughly in
order of fit:

- **[Typesense](https://typesense.org)** - open-source, REST API in the
  Algolia/Meilisearch mold (a single query string plus a filter
  expression); likely the most direct backend to add next.
- **[Quickwit](https://quickwit.io)** - a distributed search engine built
  directly on Tantivy, making it the most architecturally fitting
  addition: the remote counterpart to the embedded engine
  `rusty-search-tantivy` already wraps.
- **[Manticore Search](https://manticoresearch.com)** - a Sphinx-descended
  engine with an Elasticsearch/Solr-like REST API.
- **[Redis/RediSearch](https://redis.io/docs/latest/develop/interact/search-and-query/)** -
  in-memory, widely deployed, with its own query-string DSL.
- **SQLite FTS5** - genuinely embedded like `rusty-search-tantivy`, but via
  SQL virtual tables rather than an inverted-index library; would make the
  "point it at SQLite" comparison above literal rather than a metaphor.
- **A managed enterprise search SaaS** (e.g. Amazon Kendra, Google Vertex
  AI Search) - a different shape than `rusty-search-elasticsearch`/
  `rusty-search-opensearch`'s self-hosted-cluster model, closer to
  `rusty-search-algolia`/`rusty-search-azure-search`'s hosted-service one.
- **Vector/hybrid search** (e.g. Qdrant, Weaviate, Pinecone, Milvus) - a
  bigger undertaking than the rest of this list: none of them fit the
  current `Query` DSL (term/match/range/bool over structured fields), so
  adding one first means deciding whether `Query` grows a
  vector-similarity variant at all, not just writing a new adapter crate.

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
