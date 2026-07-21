# Architecture

## Overview
`rusty_search` is an async, pluggable search interface for Rust. Application
code is written once against a standard `SearchBackend` trait; the concrete
search engine underneath is chosen — and swappable — at construction time.
It is not a search engine itself: `rusty-search-core` defines no storage or
indexing, only the shared vocabulary and the trait every backend implements.

## Boundaries
Ports-and-adapters, applied to search engines instead of a database: one
port (`SearchBackend`), multiple adapters, each in its own crate so a
consumer only pulls in the engine(s) it actually uses.

| Port | Adapter(s) | Notes |
| ---- | ---------- | ----- |
| `SearchBackend` (`rusty-search-core`) | `MemoryBackend` (`rusty-search-memory`), `TantivyBackend` (`rusty-search-tantivy`), `ElasticsearchBackend` (`rusty-search-elasticsearch`), `MeilisearchBackend` (`rusty-search-meilisearch`) | `async-trait`-based and object-safe (`Arc<dyn SearchBackend>`) specifically so callers can swap the concrete engine at runtime, not just at compile time |
| `Query` (DSL, not a port but worth calling out) | translated per-backend: naive whole-document evaluator (memory), Tantivy `Query`/`BooleanQuery`/`RangeQuery` (tantivy), Elasticsearch Query DSL JSON over HTTP (elasticsearch), a Meilisearch filter expression string plus at most one full-text `q` (meilisearch) | callers build one `Query` tree; each backend owns its own translation, and not every backend can represent every tree (meilisearch is the strictest) |

## Structure
A Cargo workspace, one crate per boundary:

- `rusty-search-core` — the port: `Document`, `Schema`, `Query`, `SearchRequest`/`SearchResults`, `SearchBackend`. No concrete engine, no I/O.
- `rusty-search-memory` — reference adapter: dependency-free, in-memory, `O(documents)` per search. Correctness over performance; the search equivalent of pointing an ORM at SQLite.
- `rusty-search-tantivy` — production-shaped adapter: wraps Tantivy for a real embedded inverted-index engine, in-memory or on-disk.
- `rusty-search-elasticsearch` — remote adapter: a thin, hand-rolled HTTP client (`reqwest`) for an Elasticsearch/OpenSearch cluster. Keeps a small local registry (which indices it created, their field types) instead of round-tripping to the cluster for that on every call.
- `rusty-search-meilisearch` — remote adapter: wraps the official `meilisearch-sdk` crate for a Meilisearch instance, rather than hand-rolling HTTP the way the Elasticsearch adapter does (see ADR-0003 for why). Keeps the same kind of local index/field-type registry as the Elasticsearch adapter, and waits on Meilisearch's async task model internally so its `SearchBackend` methods still look synchronous to callers.
- `rusty-search` — facade crate consumers depend on; re-exports `rusty-search-core` plus each adapter behind a feature flag (`memory`, `tantivy`, `elasticsearch`, `meilisearch`), so depending on core alone costs nothing extra.

This has not been split into separate services and shouldn't be — it's a
library, not a deployable; "splitting" here means adding another adapter
crate (e.g. an OpenSearch client), not extracting a process.

## Data flow
1. Caller builds a `Schema` and calls `backend.create_index(name, schema)`.
2. Caller builds `Document`s (id + JSON-ish fields) and calls `backend.index_batch(...)`, then `backend.commit(index)` to make them visible to search.
3. Caller builds a `Query` (composable via `.and()`/`.or()`/`.not()`), wraps it in a `SearchRequest` (sort/offset/limit), and calls `backend.search(index, request)`.
4. The adapter translates `Query`/`Schema` into its own native form (nothing in `rusty-search-core` knows Tantivy or any other engine exists), executes the search, and maps native hits back into `SearchResults`/`Hit`.

## Key decisions
See [docs/adr/](./docs/adr/) for the record of individual decisions and their tradeoffs.

## Non-goals
- Not a query language/parser for end users (no SQL-like or Lucene-syntax string query) — `Query` is a Rust-native expression tree, built programmatically.
- Not a distributed search cluster or index-replication story itself — that's the responsibility of whichever backend a consumer picks (e.g. an actual Elasticsearch cluster's own sharding/replication), not something `rusty-search-core` or `rusty-search-elasticsearch` reimplements.
- `rusty-search-tantivy`'s fallback sort path (non-fast fields, multiple sort keys) is correct only up to `FALLBACK_SORT_CAP` documents — not a general-purpose distributed sort.
- `rusty-search-meilisearch` doesn't attempt to represent every `Query` tree Meilisearch can't natively express (more than one `Query::Match`, `must_not` wrapping a bare `Query::MatchAll`/`Query::Match`) — those are rejected with `SearchError::InvalidQuery` rather than approximated.
