# ADR-0001: Object-safe `SearchBackend` trait over an engine-agnostic query DSL

Status: Accepted
Date: 2026-07-21

## Context
`rusty_search` exists to let application code depend on one interface for
search while the concrete engine underneath stays swappable — an in-memory
index in tests, an embedded Tantivy index or a remote engine in
production — without touching call sites. That requires deciding, up
front, how the interface is shaped: as a trait or a struct, whether it
supports runtime swapping or only compile-time selection, and whether
callers write queries in each engine's native syntax or in something
shared.

## Decision
- Define a single async trait, `SearchBackend`, in a dependency-free
  `rusty-search-core` crate, covering index lifecycle
  (`create_index`/`delete_index`/`index_exists`), document lifecycle
  (`index`/`index_batch`/`delete`/`commit`), and `search`.
- Write it with `#[async_trait]` (the `async-trait` crate) specifically so
  it stays object-safe: callers hold `Arc<dyn SearchBackend>` and swap the
  concrete engine at *runtime*, not just at compile time.
- Route every query through a shared, composable `Query` DSL
  (`Query::term`/`match_query`/`range`, combined with `.and()`/`.or()`/
  `.not()`) plus engine-agnostic `Document`/`Schema` types, instead of
  letting callers pass each engine's native query syntax through.
- Ship each backend as its own crate in the workspace
  (`rusty-search-memory`, `rusty-search-tantivy`, ...), re-exported behind
  feature flags from a `rusty-search` facade crate, rather than one crate
  with everything behind `cfg`.

## Alternatives considered
- **Generic `SearchBackend<D>` over a document type parameter**, dispatched
  statically instead of via trait objects. Zero-cost, but forecloses
  runtime swapping — the explicit goal here, mirroring how a SQLAlchemy
  `Engine` is chosen at runtime from a connection string, not baked into
  the call site's type.
- **Native `async fn` in traits, no `async-trait` macro.** Rust's built-in
  support doesn't produce a dyn-compatible trait without the caller
  manually boxing every future; `async-trait` does that boxing for us and
  is the ecosystem-standard choice for object-safe async traits today. The
  cost is a small per-call allocation, accepted as reasonable for the
  flexibility gained.
- **Backend-native query passthrough** (callers write Tantivy query syntax,
  an Elasticsearch DSL body, etc., directly). Simpler per backend, but
  defeats the point of a standard interface: every call site would need to
  know which engine it's talking to, exactly what this project exists to
  avoid.
- **One crate, all backends behind `cfg` features**, instead of a
  workspace. Simpler to publish, but couples every backend's dependency
  tree (e.g. Tantivy's) into one `Cargo.lock` even for consumers who only
  want the in-memory backend. Splitting by crate, feature-gated from a
  facade, keeps dependency footprints isolated — the same shape `sqlx`
  uses for its database drivers.

## Consequences
- Every new backend translates the shared `Query`/`Schema`/`Document`
  vocabulary into its own native representation; the DSL's expressiveness
  is capped by what a reasonable intersection of target backends can
  support well. `rusty-search-tantivy` already documents where its
  translation is incomplete (sort support beyond a single fast numeric
  field) rather than pretending full parity.
- Adding a backend is additive — a new crate, no changes to
  `rusty-search-core` or existing callers. Widening the shared vocabulary
  itself (adding a `Query` variant, a new `FieldType`) is not additive: it
  requires updating every existing backend's translation logic.
- `Arc<dyn SearchBackend>` plus `async-trait`'s boxed futures cost a small
  amount of dispatch/allocation overhead versus monomorphized generics.
  Accepted as the right tradeoff for a library whose entire value
  proposition is runtime pluggability.
