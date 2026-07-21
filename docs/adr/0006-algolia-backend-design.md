# ADR-0006: Algolia backend design

Status: Accepted
Date: 2026-07-21

## Context
Algolia is a hosted search SaaS, not a self-hosted engine like
Elasticsearch/OpenSearch/Solr or a self-hosted server like Meilisearch -
there is no cluster or instance to point at, only an application
identified by an `app_id`, authenticated with API keys, and reachable at
per-application hostnames. Adding `rusty-search-algolia` raises the same
two questions earlier ADRs already answered for other backends: hand-roll
HTTP or lean on an existing crate (ADR-0002 vs. ADR-0003), and how much of
the `Query` DSL can this backend's native query model actually represent
(ADR-0003, ADR-0005)?

No actively-maintained, broadly-adopted async Algolia Rust client exists
on crates.io with confidence comparable to `meilisearch-sdk`'s - the
crates found (`algoliasearch`, `EasyAlgolia`, `algolia-monitoring-rs`) are
low-adoption and not obviously maintained. Algolia's REST API also has
some shape genuinely different from every other backend in this
workspace: writes are asynchronous (return a `taskID` that must be polled
for completion, the same async-task shape Meilisearch uses - see
ADR-0003), reads and writes go to *different* hostnames
(`{app_id}.algolia.net` for writes, `{app_id}-dsn.algolia.net` for reads,
the latter backed by a distributed search network for lower read
latency), and its filter language, while as arbitrarily nestable as Solr's
Lucene syntax, has no "match everything" literal (`*:*`) to ground a
negative clause against.

## Decision
- `rusty-search-algolia` is hand-rolled over `reqwest` (matching
  `rusty-search-elasticsearch`/`rusty-search-solr`'s approach, not
  `rusty-search-meilisearch`'s SDK-based one), for the same reason as
  Solr: no trustworthy async client crate to lean on.
- `AlgoliaBackend` holds separate `write_host`/`read_host` fields,
  derived from `app_id` by default (`new`/`with_client`), with a
  `with_hosts` constructor that lets both be pointed anywhere - not just
  as an escape hatch for proxy/regional deployments, but the only way to
  make this backend testable against a single mocked HTTP server at all,
  since the default constructors hard-code Algolia's real two-hostname
  production URL scheme.
- Every write operation (`create_index`, `delete_index`, `index_batch`,
  `delete`) submits its request, receives a `taskID`, and polls
  `GET /1/indexes/{index}/task/{taskID}` internally until Algolia reports
  `"status": "published"`, before returning - the same async-task-waiting
  shape ADR-0003 established for Meilisearch, and for the same reason:
  callers see a synchronous `Result<()>`-returning `SearchBackend` method,
  not a task handle to poll themselves. `commit()` is consequently a
  no-op here too, since every write is already durable by the time its
  method returns.
- `Query` translation produces a single free-text `query` string (at most
  one `Query::Match`, restricted via `restrictSearchableAttributes` to
  that one field - the same one-full-text-clause ceiling
  `rusty-search-meilisearch` has, since Algolia's search API also has
  exactly one query string per request) plus a single `filters` expression
  string for everything else. Unlike Meilisearch's filter language,
  Algolia's `filters` nests `AND`/`OR`/`NOT` arbitrarily in one string,
  the same as Solr's Lucene syntax - so `must`/`filter`/`should` all
  translate losslessly. `must_not`, however, is only representable when it
  wraps something that actually renders to an expression: `must_not`
  wrapping a bare `Query::MatchAll`/`Query::Match` is rejected with
  `SearchError::InvalidQuery`, because Algolia's filter language has no
  equivalent to Lucene's `*:*` to negate against the way
  `rusty-search-solr` does (ADR-0005's `*:* AND (<tree>)` grounding trick
  has no analog here).
- `Hit::score` is always the constant `1.0`. Algolia does expose internal
  ranking information, but not as a single portable relevance number the
  way Elasticsearch/Solr/Tantivy's `_score`/scoring do; fabricating one
  from ranking internals would be false precision. Result *order* still
  reflects Algolia's actual ranking - only the numeric score field is a
  placeholder.
- Native per-query field sorting isn't attempted. Algolia's real answer to
  custom sort order is replica indices (separate indices with different
  ranking configs), which is out of scope for a first implementation and
  a materially different model from every other backend's `Sort` handling
  here. Instead, this backend reuses the client-side fallback-sort pattern
  `rusty-search-tantivy` already established (`FALLBACK_SORT_CAP`-bounded
  in-memory sort) whenever a `Sort::Field` is requested.

## Alternatives considered
- **Depend on an existing Algolia Rust client crate**, matching
  `rusty-search-meilisearch`'s approach. Rejected for the same reason as
  Solr: nothing on crates.io met the bar `meilisearch-sdk` set for
  trustworthiness/adoption/maintenance.
- **A single derived host for both reads and writes**, avoiding the
  two-hostname complexity. Rejected: Algolia's actual API genuinely
  separates them (write host vs. distributed-search-network read host),
  and collapsing that would misrepresent how a real Algolia application
  is reached - `with_hosts` exists precisely so tests can collapse them
  without the production code path lying about the split.
- **Approximate `must_not(MatchAll)` with a tautological filter clause**
  (e.g. some always-true attribute condition), to give Solr-style
  completeness. Rejected: this workspace's stated philosophy (see
  ADR-0003, ADR-0005) is honest limitation over fragile approximation - a
  synthetic tautology would depend on undocumented Algolia filter-language
  edge cases with no live application available to verify against.
- **Skip client-side fallback sort, only support Algolia's natural
  ranking order.** Rejected: `Sort::Field` is part of the shared `Query`
  DSL surface every other backend honors to some degree (natively or via
  fallback); silently ignoring a caller's requested sort order would be a
  worse experience than a documented, capped approximation.

## Consequences
- `rusty-search-algolia` duplicates conceptually similar shape to
  `rusty-search-meilisearch` (SDK-free HTTP client would have looked
  different; task-polling instead looks like ADR-0003's Meilisearch
  logic) without sharing code, since Algolia's actual request/response
  formats (`objectID`, `/1/indexes/{name}/batch`, `filters` strings) are
  its own.
- Applications relying on Algolia's replica-index sort ordering,
  relevance-score internals, or SigV4-style advanced auth won't get that
  through this backend; the documented workarounds (`with_hosts` for
  custom endpoints, fallback sort, constant score) are honest
  approximations, not full parity with Algolia's dashboard-driven feature
  set.
- No automated test in this repo exercises a real Algolia application -
  the same disclosed gap as every other remote backend in this workspace
  (ADR-0002, ADR-0003, ADR-0005). `with_hosts` is what makes the
  `wiremock`-backed test suite possible at all here.
- `must_not` wrapping a bare `Query::MatchAll`/`Query::Match` is rejected
  outright, same as Meilisearch - portable application code targeting
  every backend in this workspace should treat that as unsupported,
  not assume Solr's extra expressiveness carries over.
