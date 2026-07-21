# Release Notes

No version tags yet — entries are tracked one per merged PR against `main`,
reverse chronological, each linking back to its PR.

---

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
