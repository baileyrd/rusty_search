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
| `SearchBackend` (`rusty-search-core`) | `MemoryBackend` (`rusty-search-memory`), `TantivyBackend` (`rusty-search-tantivy`) | `async-trait`-based and object-safe (`Arc<dyn SearchBackend>`) specifically so callers can swap the concrete engine at runtime, not just at compile time |
| `Query` (DSL, not a port but worth calling out) | translated per-backend: naive whole-document evaluator (memory), Tantivy `Query`/`BooleanQuery`/`RangeQuery` (tantivy) | callers build one `Query` tree; each backend owns its own translation |

## Structure
A Cargo workspace, one crate per boundary:

- `rusty-search-core` — the port: `Document`, `Schema`, `Query`, `SearchRequest`/`SearchResults`, `SearchBackend`. No concrete engine, no I/O.
- `rusty-search-memory` — reference adapter: dependency-free, in-memory, `O(documents)` per search. Correctness over performance; the search equivalent of pointing an ORM at SQLite.
- `rusty-search-tantivy` — production-shaped adapter: wraps Tantivy for a real embedded inverted-index engine, in-memory or on-disk.
- `rusty-search` — facade crate consumers depend on; re-exports `rusty-search-core` plus each adapter behind a feature flag (`memory`, `tantivy`), so depending on core alone costs nothing extra.

This has not been split into separate services and shouldn't be — it's a
library, not a deployable; "splitting" here means adding another adapter
crate (e.g. an Elasticsearch/Meilisearch/OpenSearch client), not extracting
a process.

## Data flow
1. Caller builds a `Schema` and calls `backend.create_index(name, schema)`.
2. Caller builds `Document`s (id + JSON-ish fields) and calls `backend.index_batch(...)`, then `backend.commit(index)` to make them visible to search.
3. Caller builds a `Query` (composable via `.and()`/`.or()`/`.not()`), wraps it in a `SearchRequest` (sort/offset/limit), and calls `backend.search(index, request)`.
4. The adapter translates `Query`/`Schema` into its own native form (nothing in `rusty-search-core` knows Tantivy or any other engine exists), executes the search, and maps native hits back into `SearchResults`/`Hit`.

## Key decisions
See [docs/adr/](./docs/adr/) for the record of individual decisions and their tradeoffs.

## Non-goals
- Not a query language/parser for end users (no SQL-like or Lucene-syntax string query) — `Query` is a Rust-native expression tree, built programmatically.
- Not a distributed search cluster or index-replication story — that's a concern for whichever backend a consumer picks (e.g. a future Elasticsearch adapter), not for `rusty-search-core`.
- `rusty-search-tantivy`'s fallback sort path (non-fast fields, multiple sort keys) is correct only up to `FALLBACK_SORT_CAP` documents — not a general-purpose distributed sort.
