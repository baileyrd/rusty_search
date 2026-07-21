# ADR-0003: Meilisearch backend on the official SDK, with a single-full-text-query restriction

Status: Accepted
Date: 2026-07-21

## Context
ADR-0002 added `rusty-search-elasticsearch` as the workspace's first remote
backend, hand-rolling HTTP calls over `reqwest`. Adding
`rusty-search-meilisearch` raises two new questions Elasticsearch didn't:
whether to hand-roll HTTP again or lean on Meilisearch's official Rust SDK,
and how to translate `Query` into Meilisearch's search API, which is
shaped very differently from Elasticsearch's - a single free-text query
string plus a separate string-expression filter language, rather than one
composable JSON query tree. Meilisearch's write operations (index
creation, settings updates, document indexing/deletion) are also all
asynchronous, returning a task to poll rather than completing inline,
which ADR-0002's Elasticsearch model (mostly-synchronous HTTP calls) didn't
have to account for.

## Decision
- Depend on the official [`meilisearch-sdk`](https://docs.rs/meilisearch-sdk)
  crate rather than hand-rolling HTTP calls, unlike
  `rusty-search-elasticsearch`. Its default transport is already
  `reqwest` + `rustls`, so this doesn't reintroduce an OpenSSL dependency
  either.
- Wait for every write operation's Meilisearch task to reach a terminal
  state (via `TaskInfo::wait_for_completion`) before the corresponding
  `SearchBackend` method returns, mapping a failed task's error the same
  way an immediate HTTP error is mapped. Because every write already waits
  this way, [`SearchBackend::commit`] is a no-op for this backend - there
  is nothing left to flush by the time it's called.
- Translate `Query` into two separate pieces, matching what Meilisearch's
  search API actually takes: at most one `Query::Match` clause becomes
  the free-text `q` (with `attributesToSearchOn` scoping it to one
  field), and everything else (`Query::Term`, `Query::Range`,
  `Query::Bool`'s boolean structure) compiles into a Meilisearch filter
  expression string. A `Query` tree containing more than one
  `Query::Match`, or a `must_not` wrapping a bare `Query::MatchAll`/
  `Query::Match`, is rejected with `SearchError::InvalidQuery` rather than
  approximated.
- Reuse the same local index/field-type registry shape ADR-0002
  introduced for Elasticsearch, for the same reason: knowing a field's
  core `FieldType` lets query translation render the right filter literal
  (a quoted string vs. a bare number/boolean) without fetching the
  index's settings back from the server.
- Restrict `Query::Range` to `I64`/`F64` fields, since Meilisearch's filter
  comparison operators (`<`, `>`, `<=`, `>=`) only work on numbers, not
  strings (including date strings) - stricter than both
  `rusty-search-tantivy` and `rusty-search-elasticsearch`, which both
  support date ranges.

## Alternatives considered
- **Hand-roll HTTP calls with `reqwest`**, matching `rusty-search-elasticsearch`
  for consistency between the two remote backends. Rejected because
  Meilisearch's task-based async model is easy to get subtly wrong by
  hand (polling cadence, timeout handling, task-failure error shapes),
  and the official SDK already implements and tests that logic. The
  inconsistency this creates - one remote backend hand-rolled, one built
  on an SDK - is an accepted, visible tradeoff, not an oversight.
- **Approximate multi-Match queries** by picking one match clause and
  silently dropping the others, or by concatenating multiple match values
  into a single `q` string. Rejected as too surprising: a caller whose
  query tree quietly searches on different terms than they asked for is
  worse than an explicit `SearchError::InvalidQuery` telling them why.
- **Represent must_not(MatchAll)/must_not(Match) via a workaround** (e.g.
  synthesizing an always-false filter from an arbitrary indexed field).
  Rejected as fragile and non-obvious; an explicit error is more honest
  about the limitation than a workaround that could break in
  non-obvious ways for schemas without a convenient field to abuse.
- **Support `Query::Range` on date-as-string fields** by requiring dates be
  stored as numeric epoch timestamps instead of RFC 3339 strings (as the
  other two backends do). Rejected for this iteration to keep `Document`
  representation consistent across backends; documented as a known
  limitation instead of a silent behavior difference in how dates are
  stored.

## Consequences
- Query trees that were valid for `rusty-search-memory`/`rusty-search-tantivy`/
  `rusty-search-elasticsearch` are not automatically valid for
  `rusty-search-meilisearch` - specifically, anything with more than one
  full-text match clause, or a negated bare match-all/match. Application
  code that needs to run identically across all four backends should
  stick to a single `Query::Match` and use `Query::Term`/`Query::Range`
  for everything else, as the `pluggable_backends` example does.
  `Query::Range` on `Date` fields also isn't portable to this backend.
- `rusty-search-meilisearch`'s dependency tree includes `meilisearch-sdk`
  and its own transitive dependencies, which don't overlap with
  `rusty-search-elasticsearch`'s `reqwest` version (the SDK currently
  pins an older `reqwest` major version) - a minor `Cargo.lock` footprint
  cost, not a functional one, since each backend is behind its own
  feature flag.
- As with `rusty-search-elasticsearch`, no automated test in this repo
  exercises a real Meilisearch instance end-to-end; the test suite mocks
  the HTTP layer (including task-polling responses) rather than running
  against live infrastructure. A live-instance smoke test remains a
  reasonable, not-yet-done follow-up, same as ADR-0002's stated gap for
  Elasticsearch.
