# Release Notes

No version tags yet — entries are tracked one per merged PR against `main`,
reverse chronological, each linking back to its PR.

---

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
