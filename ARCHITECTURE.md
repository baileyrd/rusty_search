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
| `SearchBackend` (`rusty-search-core`) | `MemoryBackend` (`rusty-search-memory`), `TantivyBackend` (`rusty-search-tantivy`), `ElasticsearchBackend` (`rusty-search-elasticsearch`), `OpenSearchBackend` (`rusty-search-opensearch`), `MeilisearchBackend` (`rusty-search-meilisearch`), `SolrBackend` (`rusty-search-solr`), `AlgoliaBackend` (`rusty-search-algolia`), `AzureSearchBackend` (`rusty-search-azure-search`) | `async-trait`-based and object-safe (`Arc<dyn SearchBackend>`) specifically so callers can swap the concrete engine at runtime, not just at compile time |
| `Query` (DSL, not a port but worth calling out) | translated per-backend: naive whole-document evaluator (memory), Tantivy `Query`/`BooleanQuery`/`RangeQuery` (tantivy), Elasticsearch Query DSL JSON over HTTP (elasticsearch, reused as-is by opensearch), a Meilisearch filter expression string plus at most one full-text `q` (meilisearch), a Solr Lucene query string plus `fq` filters (solr), an Algolia `filters` expression string plus at most one full-text `query` (algolia), an Azure full-Lucene-syntax `search` string plus an OData `$filter` (azure-search) | callers build one `Query` tree; each backend owns its own translation (opensearch reuses elasticsearch's rather than duplicating it), and not every backend can represent every tree (meilisearch and algolia are the strictest on full-text, solr and azure-search the most complete overall) |

## Structure
A Cargo workspace, one crate per boundary:

- `rusty-search-core` — the port: `Document`, `Schema`, `Query`, `SearchRequest`/`SearchResults`, `SearchBackend`. No concrete engine, no I/O.
- `rusty-search-memory` — reference adapter: dependency-free, in-memory, `O(documents)` per search. Correctness over performance; the search equivalent of pointing an ORM at SQLite.
- `rusty-search-tantivy` — production-shaped adapter: wraps Tantivy for a real embedded inverted-index engine, in-memory or on-disk.
- `rusty-search-elasticsearch` — remote adapter: a thin, hand-rolled HTTP client (`reqwest`) for an Elasticsearch cluster. Keeps a small local registry (which indices it created, their field types) instead of round-tripping to the cluster for that on every call.
- `rusty-search-opensearch` — remote adapter: a thin wrapper around `ElasticsearchBackend`, not an independent implementation. OpenSearch still speaks Elasticsearch's wire protocol for everything this workspace needs, so this adapter reuses that translation logic entirely instead of duplicating it (see ADR-0004).
- `rusty-search-meilisearch` — remote adapter: wraps the official `meilisearch-sdk` crate for a Meilisearch instance, rather than hand-rolling HTTP the way the Elasticsearch adapter does (see ADR-0003 for why). Keeps the same kind of local index/field-type registry as the Elasticsearch adapter, and waits on Meilisearch's async task model internally so its `SearchBackend` methods still look synchronous to callers.
- `rusty-search-solr` — remote adapter: a thin, hand-rolled HTTP client (`reqwest`) for an Apache Solr instance, independent from the Elasticsearch adapter despite the surface similarity (see ADR-0005 for why this one isn't a wrapper the way OpenSearch's is). Keeps the same kind of local index/field-type registry, and translates `Query` into a Lucene query string plus separate `fq` filter queries.
- `rusty-search-algolia` — remote adapter: a thin, hand-rolled HTTP client (`reqwest`) for the hosted Algolia search SaaS, since no trustworthy async Algolia Rust SDK exists on crates.io (see ADR-0006). Keeps the same kind of local index/field-type registry as the other remote adapters, waits on Algolia's async task model internally (making `commit()` a no-op, the same pattern ADR-0003 established for Meilisearch), and translates `Query` into a single `filters` expression string plus at most one full-text `query`.
- `rusty-search-azure-search` — remote adapter: a thin, hand-rolled HTTP client (`reqwest`) for the hosted Azure AI Search service, for the same reason as Solr/Algolia (see ADR-0007). Keeps the same kind of local index/field-type registry as the other remote adapters; writes are synchronous over HTTP with no task to wait on, making `commit()` a no-op for a different reason than Meilisearch/Algolia's (there's simply nothing to flush, not "already flushed"). Translates `Query` into a full-Lucene-syntax `search` string (as expressive as Solr's `q`) plus a genuinely separate OData `$filter`; sorting requires a field to be marked `sortable` at index-creation time, mirroring `rusty-search-tantivy`'s fast fields rather than Elasticsearch's everything-sortable default.
- `rusty-search` — facade crate consumers depend on; re-exports `rusty-search-core` plus each adapter behind a feature flag (`memory`, `tantivy`, `elasticsearch`, `opensearch`, `meilisearch`, `solr`, `algolia`, `azure-search`), so depending on core alone costs nothing extra.

This has not been split into separate services and shouldn't be — it's a
library, not a deployable; "splitting" here means adding another adapter
crate (e.g. a client for a hosted search SaaS), not extracting a process.

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
- `rusty-search-opensearch` doesn't implement AWS SigV4 request signing for Amazon OpenSearch Service; `OpenSearchBackend::with_client` is the escape hatch until it does.
- `rusty-search-solr` creates cores via the Core Admin API against the `_default` configset - it doesn't support SolrCloud's Collections API, so it won't work against a cloud-mode cluster.
- `rusty-search-algolia` rejects the same `Query` shapes `rusty-search-meilisearch` does (more than one `Query::Match`, `must_not` wrapping a bare `Query::MatchAll`/`Query::Match` - Algolia's `filters` language has no "match everything" literal to negate against), has no native per-query field sort (falls back to the same in-memory `FALLBACK_SORT_CAP`-bounded sort as `rusty-search-tantivy`), and has no native relevance score - every `Hit::score` is a constant `1.0`, with result *order* (not score value) reflecting Algolia's actual ranking.
- `rusty-search-azure-search` rejects `Query::Match` inside `Query::Bool::filter` (OData has no full-text primitive) and `must_not` wrapping a bare `Query::MatchAll` outside `Query::Bool::filter` (unlike Solr's `*:*`, Azure's bare `*` is only trusted at the outermost position); `Query::Range` is restricted to `I64`/`F64`/`Date`; a field is only usable in a native `$orderby` clause if created with `FieldOptions::fast(true)`, otherwise `Sort::Field` falls back to the same in-memory `FALLBACK_SORT_CAP`-bounded sort as `rusty-search-tantivy`/`rusty-search-algolia`; and there's no support for Azure Active Directory/managed-identity auth, only `api-key`.
