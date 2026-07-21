# Changelog

All notable changes to this repo are documented here.
Format: Added / Changed / Deprecated / Removed / Fixed / Security, newest first.

## [Unreleased]
### Added
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

<!-- ## [0.1.0] - YYYY-MM-DD
### Added
- Initial release -->
