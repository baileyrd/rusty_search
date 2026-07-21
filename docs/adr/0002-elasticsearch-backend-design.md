# ADR-0002: Elasticsearch backend as a thin HTTP client with a local index registry

Status: Accepted
Date: 2026-07-21

## Context
ADR-0001 established `SearchBackend` as an object-safe async trait over a
shared `Query`/`Schema`/`Document` vocabulary, with `rusty-search-memory`
and `rusty-search-tantivy` as the first two adapters - both in-process
engines. Adding `rusty-search-elasticsearch` is the first adapter for a
*remote* engine, which raises questions the first two didn't: how to talk
to it (HTTP client choice), how to know a field's type without an extra
round trip on every query, and how to test a backend that needs a live
cluster to fully exercise.

## Decision
- Use [`reqwest`](https://docs.rs/reqwest) with `rustls` (not `native-tls`)
  as the HTTP client, so the crate builds without a system OpenSSL
  dependency.
- Keep a small local registry (`Arc<RwLock<HashMap<String, FieldMap>>>`)
  of indices created through this `SearchBackend` instance, recording each
  field's core `FieldType`. Query/range translation coerces a `Query`'s
  string/JSON values into the right JSON type from this map, instead of
  fetching the index's mapping back from the cluster on every call.
  `index_exists` and "index not found" errors reflect this local registry,
  not the cluster's actual state - consistent with the other two backends,
  where index lifecycle is expected to go entirely through the trait.
- Generate document ids client-side (`uuid::Uuid::new_v4()`) when a
  `Document` doesn't have one, same as `rusty-search-tantivy`, rather than
  letting Elasticsearch assign one and parsing it back out of the bulk
  response.
- Map `Query::Bool`'s `filter` clauses directly onto Elasticsearch's `bool`
  query `filter` context - a real non-scoring filter, unlike
  `rusty-search-tantivy`, which has to fold `filter` into `must` because
  Tantivy's `BooleanQuery` has no equivalent.
- Test against a mocked HTTP server ([`wiremock`](https://docs.rs/wiremock))
  rather than a live cluster, asserting on requests sent and responses
  parsed for every `SearchBackend` method.

## Alternatives considered
- **`native-tls` instead of `rustls`.** Would link against the system's
  OpenSSL, adding a build-time dependency this workspace doesn't otherwise
  need and complicating cross-compilation. `rustls` has no such dependency.
- **Fetch the index mapping from Elasticsearch on every query** instead of
  keeping a local field-type map. Simpler state management, but adds a
  network round trip to every single search/term/range query purely to
  answer "what type is this field" - a question we already know the answer
  to from `create_index`.
- **Let Elasticsearch assign document ids** and read them back out of the
  bulk response. Avoids a UUID dependency, but breaks the invariant every
  other backend gives callers: the id is known immediately after
  `index`/`index_batch` returns, not after parsing a bulk response.
- **Require a live Elasticsearch cluster for tests** (e.g. via Docker).
  More representative, but ties every test run - and CI - to
  infrastructure this workspace otherwise has no need for. A mocked HTTP
  server verifies the same request/response contract without it; a
  live-cluster integration test remains a reasonable follow-up but isn't a
  blocker for this backend's correctness on the seams that matter (request
  shape, response parsing, error mapping).

## Consequences
- `rusty-search-elasticsearch` cannot see indices created by any other
  client (a different backend instance, `curl`, Kibana) - only ones it
  created itself. Multi-process or multi-instance coordination against the
  same cluster is out of scope for this backend as it stands.
- Because `filter` is genuinely non-scoring here, the exact same `Query`
  tree can score differently across backends (Tantivy folds `filter` into
  `must`, contributing to score; Elasticsearch doesn't). This was already
  true in spirit - `rusty-search-tantivy`'s docs call out that its `filter`
  handling is an approximation - but it's worth stating plainly: score
  values are not meant to be compared *across* backends, only used to rank
  results *within* one search call.
- No automated test in this repo currently exercises a real Elasticsearch
  cluster end-to-end; the mocked test suite covers the HTTP contract this
  backend implements, not Elasticsearch's own behavior (query parsing
  edge cases, cluster health, mapping conflicts). A live-cluster smoke test
  (e.g. behind a feature flag or a Docker Compose file) is a reasonable
  future addition, tracked as a known gap rather than left implicit.
