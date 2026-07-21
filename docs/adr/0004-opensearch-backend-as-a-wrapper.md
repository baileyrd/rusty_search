# ADR-0004: OpenSearch backend wraps ElasticsearchBackend instead of reimplementing it

Status: Accepted
Date: 2026-07-21

## Context
OpenSearch is a fork of Elasticsearch (pre-7.11), created by AWS after
Elastic's license change. For every operation `SearchBackend` needs -
index creation and mappings, bulk indexing, document deletion, refresh,
and the Query DSL used for search - OpenSearch still speaks the same wire
protocol Elasticsearch does today. Adding `rusty-search-opensearch` raises
a question the other backends didn't: given `rusty-search-elasticsearch`
already implements that exact wire protocol, correctly, with a full test
suite, is there any real value in writing a second, independent
implementation of the same request/response translation against an
effectively identical API?

## Decision
- `rusty-search-opensearch` depends on `rusty-search-elasticsearch` and
  defines `OpenSearchBackend` as a thin newtype wrapper around
  `ElasticsearchBackend`, delegating every `SearchBackend` method to the
  wrapped instance. It does not reimplement schema/query/document
  translation.
- `OpenSearchBackend` still gets its own type, its own crate, and its own
  `opensearch` feature flag on the `rusty-search` facade - so a consumer
  who wants OpenSearch doesn't need to reason about Elasticsearch at all,
  even though the two share an implementation underneath.
- Constructors mirror `ElasticsearchBackend`'s (`new`, `with_basic_auth`,
  `with_client`), with one intentional omission: no `with_api_key`, since
  Elasticsearch's `Authorization: ApiKey <key>` scheme isn't a standard
  OpenSearch security-plugin mechanism the way HTTP basic auth is.
- The wrapper's own test suite is intentionally small (proving
  construction and delegation reach the server correctly) rather than
  re-covering every query/schema translation case ADR-0002's test suite
  already covers - those tests apply unchanged, since the code path is
  identical.

## Alternatives considered
- **A second independent implementation**, duplicating
  `rusty-search-elasticsearch`'s `schema_map`/`query_map`/`convert`
  modules (and their ~600 lines and 27 tests) under a new crate. Rejected:
  the two APIs aren't just similar, they're the same wire protocol for
  everything this workspace uses, so duplicating the translation logic
  would only create two copies of the same code to keep in sync, with no
  corresponding benefit - divergence between the "duplicates" would be a
  bug, not a feature.
- **A type alias** (`pub type OpenSearchBackend = ElasticsearchBackend;`).
  Simpler than a wrapper, but forecloses ever diverging - which OpenSearch
  and Elasticsearch are increasingly likely to do over time (OpenSearch's
  own security plugin, its k-NN vector search extensions, its own
  auth model). A newtype wrapper costs a few lines of delegation now and
  buys room to add OpenSearch-specific behavior later without breaking
  the type identity anyone using `rusty-search-opensearch` depends on.
- **No separate crate at all** - just document that
  `rusty-search-elasticsearch`'s `ElasticsearchBackend` also happens to
  work against OpenSearch. Rejected because it puts the burden of knowing
  that on every consumer, rather than on this workspace: a user reaching
  for "an OpenSearch backend" should find one named that, matching how
  they'd search for it, rather than being expected to already know
  OpenSearch and Elasticsearch share a wire protocol.
- **Implement AWS SigV4 signing now**, since Amazon OpenSearch Service (the
  most common managed OpenSearch deployment) requires it rather than
  basic auth. Deferred: correctly implementing and testing request signing
  without access to a real AWS-managed cluster is a meaningfully separate
  effort from the wrapper itself, and getting it subtly wrong is worse
  than not having it. `with_client` is documented as the interim escape
  hatch - a caller can layer SigV4 signing into their own `reqwest::Client`
  today.

## Consequences
- `rusty-search-opensearch` inherits every behavior and limitation
  `rusty-search-elasticsearch` has today, including its known limitations
  (the local index/field-type registry, `filter` clauses' scoring
  semantics, etc.) - documented once, in the Elasticsearch crate, and
  referenced rather than restated.
- If OpenSearch's API ever meaningfully diverges from Elasticsearch's for
  an operation this trait uses, `rusty-search-opensearch` will need actual
  logic of its own at that point (no longer a pure wrapper) - an explicit,
  anticipated future cost of this choice, not a surprise.
- There is currently no AWS SigV4 support, which most real Amazon
  OpenSearch Service deployments need; `with_client` is a real but
  unpolished escape hatch, tracked here rather than silently absent.
