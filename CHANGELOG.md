# Changelog

All notable changes to this repo are documented here.
Format: Added / Changed / Deprecated / Removed / Fixed / Security, newest first.

## [Unreleased]
### Added
- CI workflow (`.github/workflows/ci-rust.yml`): `cargo fmt --check`,
  `cargo clippy --all-targets --all-features -- -D warnings`, and
  `cargo test --all-features` on every PR and push to `main`. (#12)
- Initial `rusty_search` workspace: `rusty-search-core` (the `SearchBackend`
  trait, `Document`, `Schema`, a composable `Query` DSL,
  `SearchRequest`/`SearchResults`), `rusty-search-memory` (dependency-free
  in-memory backend), `rusty-search-tantivy` (embedded Tantivy backend,
  in-memory or on-disk), and the `rusty-search` facade crate with
  `memory`/`tantivy` feature flags. (#1)
- Repo governance docs: PR/issue templates, CONTRIBUTING, CODE_OF_CONDUCT,
  SECURITY, ARCHITECTURE, RELEASE_NOTES. (#1)
- ADR-0001: object-safe `SearchBackend` trait over a shared query DSL. (#3)
- `rusty-search-elasticsearch`: a `SearchBackend` for a remote
  Elasticsearch/OpenSearch cluster over HTTP, wired into the `rusty-search`
  facade behind a new `elasticsearch` feature flag. (#6)
- ADR-0002: Elasticsearch backend design (local index/field-type registry,
  client-side id generation, genuinely non-scoring `filter` clauses). (#6)
- `rusty-search-meilisearch`: a `SearchBackend` for a remote Meilisearch
  instance built on the official `meilisearch-sdk` crate, wired into the
  `rusty-search` facade behind a new `meilisearch` feature flag. (#7)
- ADR-0003: Meilisearch backend design (official SDK over hand-rolled
  HTTP, async task-waiting making `commit()` a no-op, single-full-text-query
  restriction). (#7)
- `rusty-search-opensearch`: a `SearchBackend` for a remote OpenSearch
  cluster, wrapping `ElasticsearchBackend` rather than reimplementing its
  translation logic, wired into the `rusty-search` facade behind a new
  `opensearch` feature flag. (#8)
- ADR-0004: OpenSearch backend as a thin wrapper around
  `ElasticsearchBackend` instead of an independent reimplementation. (#8)
- `rusty-search-solr`: a `SearchBackend` for a remote Apache Solr
  instance, an independent implementation translating `Query` into a
  Lucene query string plus `fq` filters, wired into the `rusty-search`
  facade behind a new `solr` feature flag. (#9)
- ADR-0005: Solr backend as an independent implementation rather than a
  wrapper, contrasted with ADR-0004's OpenSearch decision. (#9)
- `rusty-search-algolia`: a `SearchBackend` for the hosted Algolia search
  SaaS, hand-rolled over `reqwest`, wired into the `rusty-search` facade
  behind a new `algolia` feature flag. (#10)
- ADR-0006: Algolia backend design (hand-rolled HTTP, async task-waiting
  making `commit()` a no-op, single-full-text-query restriction, no
  "match everything" literal to ground `must_not` against). (#10)
- `rusty-search-azure-search`: a `SearchBackend` for the hosted Azure AI
  Search service, hand-rolled over `reqwest`, translating `Query` into a
  full-Lucene-syntax `search` string plus a separate OData `$filter`,
  wired into the `rusty-search` facade behind a new `azure-search`
  feature flag. (#11)
- ADR-0007: Azure AI Search backend design (hand-rolled HTTP, two
  independent query grammars in one request, `sortable` mirroring
  Tantivy's fast fields, synchronous writes making `commit()` a no-op for
  a different reason than Meilisearch/Algolia's). (#11)

<!-- ## [0.1.0] - YYYY-MM-DD
### Added
- Initial release -->
