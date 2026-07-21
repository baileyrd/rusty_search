# Release Notes

No version tags yet — entries are tracked one per merged PR against `main`,
reverse chronological, each linking back to its PR.

---

## PR #11 — Add an Azure AI Search backend
**2026-07-21** · [#11](https://github.com/baileyrd/rusty_search/pull/11)

- **Added:** `rusty-search-azure-search`, a `SearchBackend` for the hosted
  Azure AI Search service - hand-rolled over `reqwest` (like
  `rusty-search-elasticsearch`/`rusty-search-solr`/`rusty-search-algolia`),
  since no trustworthy async Azure AI Search Rust SDK exists on crates.io
  (see ADR-0007). Wired into the `rusty-search` facade behind a new
  `azure-search` feature, and into the `pluggable_backends` example
  (skipped gracefully without
  `RUSTY_SEARCH_AZURE_SEARCH_ENDPOINT`/`RUSTY_SEARCH_AZURE_SEARCH_API_KEY`
  set).
- **Added:** `Query` translation split across Azure's two independent
  query grammars in one request - a full-Lucene-syntax `search` string
  (`queryType: "full"`, as expressive as Solr's `q`: more than one
  `Query::Match`, `must_not` wrapping `Query::Match`, using the same
  grounding trick ADR-0005 established for Solr) plus a genuinely separate
  OData `$filter` for `Query::Bool::filter` (which rejects `Query::Match`
  - OData has no full-text primitive - but *does* support `must_not`
  wrapping a bare `Query::MatchAll` via OData's real `true`/`false`
  literals, a narrower boundary than Solr's full completeness but broader
  than Meilisearch/Algolia's).
- **Added:** `FieldOptions::fast` now does something in a remote backend
  for the first time - it maps onto Azure's `sortable` attribute, which
  (like a Tantivy fast field) must be declared at index-creation time
  before native `$orderby` sorting works. A `SearchRequest` sorting by a
  non-sortable field falls back to the same `FALLBACK_SORT_CAP`-bounded
  in-memory sort `rusty-search-tantivy`/`rusty-search-algolia` already
  use.
- **Added:** ADR-0007, documenting the hand-rolled-over-SDK choice, the
  two-grammar query design and its exact completeness boundary, the
  `sortable`/fast-field parallel, and why `commit()` is a no-op here for a
  different reason than Meilisearch/Algolia's (writes are synchronous
  with nothing to poll; Azure simply has no refresh/commit concept at
  all).
- Known limitations, stated plainly: the mandatory key field is always
  named `"id"` and Azure's character restrictions on key values aren't
  validated client-side; `Query::Range` is restricted to
  `I64`/`F64`/`Date` fields; no Azure Active Directory/managed-identity
  auth, only `api-key`.
- 41 new unit tests (28 pure translation tests + 13 mocked-HTTP
  integration tests); all passed alongside the existing 150 unit tests +
  3 doctests across the workspace. `cargo clippy` and `cargo fmt --check`
  are both clean.

## PR #10 — Add an Algolia backend
**2026-07-21** · [#10](https://github.com/baileyrd/rusty_search/pull/10)

- **Added:** `rusty-search-algolia`, a `SearchBackend` for the hosted
  Algolia search SaaS - hand-rolled over `reqwest` (like
  `rusty-search-elasticsearch`/`rusty-search-solr`), since no trustworthy
  async Algolia Rust SDK exists on crates.io (see ADR-0006). Wired into
  the `rusty-search` facade behind a new `algolia` feature, and into the
  `pluggable_backends` example (skipped gracefully without
  `RUSTY_SEARCH_ALGOLIA_APP_ID`/`RUSTY_SEARCH_ALGOLIA_API_KEY` set).
- **Added:** `Query` translation into a single free-text `query` string
  (at most one `Query::Match`, restricted via
  `restrictSearchableAttributes` - the same one-full-text-clause ceiling
  as `rusty-search-meilisearch`) plus a single `filters` expression
  string for everything else. Algolia's filter language nests
  `AND`/`OR`/`NOT` arbitrarily in one string like Solr's Lucene syntax,
  but - unlike Solr - has no "match everything" literal to ground a
  negative clause against, so `must_not` wrapping a bare
  `Query::MatchAll`/`Query::Match` is rejected the same way Meilisearch
  rejects it.
- **Added:** ADR-0006, documenting the hand-rolled-over-SDK choice, the
  async task-polling write model (making `commit()` a no-op, the same
  shape ADR-0003 established for Meilisearch), the dual write/read
  hostname design (and the `with_hosts` constructor that makes both
  collapsible for testing), the constant `1.0` relevance score (Algolia
  exposes no portable single score), and the client-side fallback sort
  reused from `rusty-search-tantivy` (Algolia's native answer to custom
  sort is replica indices, out of scope here).
- Known limitations, stated plainly: no native per-query field sort
  (falls back to the same `FALLBACK_SORT_CAP`-bounded in-memory sort as
  `rusty-search-tantivy`); no native relevance score (`Hit::score` is
  always `1.0`, though result *order* still reflects Algolia's actual
  ranking); `Query::Range` restricted to `I64`/`F64` fields; no
  multi-host failover; index-exists semantics are local-registry-only,
  same caveat as every other remote backend here.
- 27 new unit tests (17 pure translation tests + 10 mocked-HTTP
  integration tests); all passed alongside the existing 123 unit tests +
  3 doctests across the workspace. `cargo clippy` and `cargo fmt --check`
  are both clean.

## PR #9 — Add a Solr backend
**2026-07-21** · [#9](https://github.com/baileyrd/rusty_search/pull/9)

- **Added:** `rusty-search-solr`, a `SearchBackend` for a remote Apache
  Solr instance - an independent implementation (hand-rolled `reqwest`,
  like `rusty-search-elasticsearch`), not a wrapper, since Solr's REST API
  isn't wire-compatible with Elasticsearch's the way OpenSearch's is (see
  ADR-0005 for the contrast with ADR-0004's OpenSearch decision). Wired
  into the `rusty-search` facade behind a new `solr` feature, and into the
  `pluggable_backends` example (skipped gracefully without
  `RUSTY_SEARCH_SOLR_URL` set).
- **Added:** `Query` translation into a single Lucene query string (`q`)
  plus separate `fq` filter queries - Solr's own genuinely non-scoring
  filter mechanism. Because Lucene's syntax supports arbitrary boolean
  nesting in one string, this backend can represent the *entire* `Query`
  DSL, including cases `rusty-search-meilisearch` has to reject (more than
  one `Query::Match`, `must_not` wrapping a bare `Query::MatchAll`).
- **Added:** ADR-0005, documenting why this backend is independent rather
  than a wrapper (Solr and Elasticsearch don't share a wire protocol, so
  there's nothing to reuse), the alternatives considered (wrapping ES
  anyway, an SDK-based approach, Solr's newer JSON Request API, SolrCloud's
  Collections API), and the consequences (no code sharing with the ES
  backend despite conceptual similarity; most expressive backend in the
  workspace, not necessarily the most portable one).
- Known limitations, stated plainly: `create_index` only supports
  standalone Solr via the Core Admin API against the `_default`
  configset, not SolrCloud's Collections API; `Query::Match` compiles to a
  quoted phrase query (analyzed, not an OR-of-terms match the way
  Elasticsearch's `match` defaults to); `Query::Range` doesn't support
  `Keyword`/`Text`/`Bool` fields. Response parsing defensively checks for
  an embedded `"error"` object before trusting the HTTP status code,
  since Solr's status-code passthrough has historically been inconsistent
  across deployments - a safe default made without a live server to
  confirm against, tracked honestly as a judgment call.
- 30 new unit tests (20 pure translation tests + 10 mocked-HTTP
  integration tests); all passed alongside the existing 93 unit tests + 3
  doctests across the workspace. `cargo clippy` and `cargo fmt --check`
  are both clean.

## PR #8 — Add an OpenSearch backend
**2026-07-21** · [#8](https://github.com/baileyrd/rusty_search/pull/8)

- **Added:** `rusty-search-opensearch`, a `SearchBackend` for a remote
  OpenSearch cluster. Rather than duplicating
  `rusty-search-elasticsearch`'s request/response translation against an
  effectively identical wire protocol, `OpenSearchBackend` is a thin
  newtype wrapper delegating every method to an inner `ElasticsearchBackend`
  - see ADR-0004 for the full reasoning. Wired into the `rusty-search`
  facade behind a new `opensearch` feature, and into the
  `pluggable_backends` example (skipped gracefully without
  `RUSTY_SEARCH_OS_URL` set).
- **Added:** ADR-0004, documenting why this backend wraps rather than
  reimplements, the alternatives considered (a second independent
  implementation, a type alias, no dedicated crate at all), and the
  consequences of that choice (inherits the Elasticsearch backend's
  limitations wholesale; would need real logic of its own if OpenSearch's
  API ever meaningfully diverges).
- Known limitation, stated plainly: no AWS SigV4 request signing for
  Amazon OpenSearch Service, the most common managed deployment target.
  `OpenSearchBackend::with_client` accepts a pre-configured
  `reqwest::Client` as the interim escape hatch.
- 6 new unit tests, deliberately scoped to proving the delegation itself
  is correct (construction, request round trips, error mapping, basic
  auth) rather than re-covering `rusty-search-elasticsearch`'s own
  query/schema/document translation tests, which apply unchanged since
  the code path is identical. All passed alongside the existing 87 unit
  tests + 3 doctests across the workspace. `cargo clippy` and
  `cargo fmt --check` are both clean.

## PR #7 — Add a Meilisearch backend
**2026-07-21** · [#7](https://github.com/baileyrd/rusty_search/pull/7)

- **Added:** `rusty-search-meilisearch`, a `SearchBackend` implementation
  for a remote Meilisearch instance, built on the official
  `meilisearch-sdk` crate rather than hand-rolled HTTP (a deliberate
  departure from `rusty-search-elasticsearch`'s approach - see ADR-0003).
  Wired into the `rusty-search` facade behind a new `meilisearch` feature,
  and into the `pluggable_backends` example (skipped gracefully without
  `RUSTY_SEARCH_MEILI_URL` set).
- **Added:** ADR-0003, documenting why this backend uses the official SDK
  instead of hand-rolled HTTP, waits on Meilisearch's async task model
  internally (making `commit()` a no-op), and restricts `Query` trees to
  at most one `Query::Match` clause plus a filter-expression translation
  of everything else - Meilisearch's search API has exactly one free-text
  query string, unlike Elasticsearch's composable query DSL.
- Known limitation, stated plainly: a `Query` tree with more than one
  `Query::Match`, or a `must_not` wrapping a bare `Query::MatchAll`/
  `Query::Match`, is rejected with `SearchError::InvalidQuery` rather than
  approximated. `Query::Range` is restricted to `I64`/`F64` fields here
  (Meilisearch filter comparisons don't support date strings the way the
  other backends' range queries do), and `SearchResults::total` reflects
  Meilisearch's `estimatedTotalHits`, not a guaranteed exact count.
- 25 new unit tests (17 pure translation tests + 8 mocked-HTTP integration
  tests covering the task-polling lifecycle); all passed alongside the
  existing 59 unit tests + 3 doctests across the workspace. `cargo clippy`
  and `cargo fmt --check` are both clean.

## PR #6 — Add an Elasticsearch backend
**2026-07-21** · [#6](https://github.com/baileyrd/rusty_search/pull/6)

- **Added:** `rusty-search-elasticsearch`, a `SearchBackend` implementation
  that talks to a remote Elasticsearch/OpenSearch cluster over HTTP via
  `reqwest` (rustls, no OpenSSL dependency). Wired into the `rusty-search`
  facade behind a new `elasticsearch` feature, and into the
  `pluggable_backends` example (skipped gracefully unless
  `RUSTY_SEARCH_ES_URL` is set, since it's the first backend needing a live
  external service).
- **Added:** ADR-0002, documenting the Elasticsearch-specific design
  choices — a local index/field-type registry instead of round-tripping to
  the cluster per query, client-side id generation matching the other
  backends, and `Query::Bool`'s `filter` mapping onto a genuinely
  non-scoring Elasticsearch `filter` context (unlike the Tantivy backend,
  which has to approximate it).
- Known limitation, stated plainly: this backend's local registry only
  knows about indices it created itself - an index created by another
  client against the same cluster won't be visible to it. Test coverage is
  against a mocked HTTP server (`wiremock`), not a live cluster; a
  live-cluster smoke test is a reasonable follow-up, not yet done.
- 27 new unit tests (16 pure translation tests + 11 mocked-HTTP integration
  tests); all passed alongside the existing 32 unit tests + 3 doctests across the workspace.
  `cargo clippy` and `cargo fmt --check` are both clean.

## PR #1 — Build rusty_search: async, pluggable search interface for Rust
**2026-07-21** · [#1](https://github.com/baileyrd/rusty_search/pull/1)

- **Added:** the initial `rusty_search` workspace — `rusty-search-core` (the
  `SearchBackend` trait, `Document`, `Schema`, a composable `Query` DSL,
  `SearchRequest`/`SearchResults`), `rusty-search-memory` (a dependency-free
  in-memory backend), `rusty-search-tantivy` (an embedded
  [Tantivy](https://github.com/quickwit-oss/tantivy) backend, in-memory or
  on-disk), and the `rusty-search` facade crate gating each backend behind a
  feature flag (`memory`, `tantivy`).
- **Added:** repo governance scaffolding — PR/issue templates,
  `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
  `ARCHITECTURE.md` (boundary table filled in for the real
  core/memory/tantivy/facade split), and an ADR seed.
- Known limitation, stated plainly rather than left implied:
  `rusty-search-tantivy`'s native sort acceleration only covers a single
  `Sort::Field` on an `i64`/`f64` field created with `fast: true`. Sorting
  by a `Keyword`/`Text`/`Bool`/`Date` field, or by more than one key, falls
  back to an in-memory sort over a candidate set capped at
  `FALLBACK_SORT_CAP` (10,000 documents) — correct up to that cap, not
  beyond it.
- Known limitation: `TantivyBackend::on_disk` does not reopen indices that
  already exist on disk from a previous process — `create_index` always
  creates fresh segments and errors if the directory already holds one.
- 32 new unit tests + 3 doctests; all passed. `cargo clippy` and
  `cargo fmt --check` are both clean across the workspace.
