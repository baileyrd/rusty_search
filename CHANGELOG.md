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

<!-- ## [0.1.0] - YYYY-MM-DD
### Added
- Initial release -->
