# ADR-0005: Solr backend as an independent implementation, not a wrapper

Status: Accepted
Date: 2026-07-21

## Context
ADR-0004 gave `rusty-search-opensearch` a specific shape - a thin wrapper
around `ElasticsearchBackend` - because OpenSearch and Elasticsearch
genuinely share a wire protocol for everything this workspace's
`SearchBackend` trait needs. Apache Solr is also a mature, Lucene-based
search engine, on the surface a similar kind of "remote HTTP search
engine" as Elasticsearch/OpenSearch. Adding `rusty-search-solr` raises the
same question ADR-0004 answered for OpenSearch: wrap an existing backend,
or build an independent one? Unlike OpenSearch, Solr's REST API is *not*
wire-compatible with Elasticsearch's: index/core lifecycle goes through
the Core Admin API and a separate Schema API rather than a single mapping
document, updates go through a JSON command format (`{"add": {"doc":
...}}` rather than the bulk NDJSON format), and search uses the classic
`q`/`fq` query-parameter model with Lucene query syntax rather than a
JSON Query DSL body.

## Decision
- `rusty-search-solr` is an independent implementation - its own
  `schema_map`/`query_map`/`convert` modules, hand-rolled over `reqwest`
  (matching `rusty-search-elasticsearch`'s approach, not
  `rusty-search-meilisearch`'s SDK-based one, since there's no
  comparably mature async Rust Solr client to lean on).
- `create_index` uses the Core Admin API (`action=CREATE`) against the
  `_default` configset, then the Schema API (`add-field`) to add the
  schema's fields. This targets standalone Solr, not SolrCloud's
  Collections API - a deliberate scope cut, not an oversight (see Known
  limitations in the crate's module docs).
- `Query` translation produces a single Lucene query string (`q`) *plus* a
  separate list of filter queries (`fq`) - Solr's own genuinely
  non-scoring filter mechanism, mirroring how `rusty-search-elasticsearch`
  maps `Query::Bool::filter` onto Elasticsearch's real `filter` context.
  Because Lucene's query syntax supports arbitrary `AND`/`OR`/`NOT`
  nesting in one string, this backend can represent an entire `Query`
  tree - including more than one `Query::Match` and `must_not` wrapping a
  bare `Query::MatchAll`/`Query::Match` - that `rusty-search-meilisearch`
  has to reject outright. The final `q` is always grounded as
  `*:* AND (<tree>)` (or bare `*:*` for a trivial tree) so a lone negative
  clause parses as a well-formed query instead of an invalid bare `NOT`.
- Response parsing defensively checks for an embedded `"error"` object in
  the JSON body *before* trusting the HTTP status code, because Solr's
  passthrough of the real status code to the HTTP layer has historically
  been inconsistent across deployments/configurations - safer to check
  the body first and fall back to the status code than to risk missing a
  failure Solr reported only inside a `200 OK`.

## Alternatives considered
- **Wrap `ElasticsearchBackend`**, matching ADR-0004's approach for
  OpenSearch. Rejected: unlike OpenSearch, Solr does not speak
  Elasticsearch's wire protocol at all. There is no shared request/response
  shape to reuse - a "wrapper" would still have to reimplement every
  translation from scratch, at which point it isn't actually reusing
  anything and the wrapper indirection just adds a layer with no benefit.
- **Depend on an existing Solr Rust client crate**, matching
  `rusty-search-meilisearch`'s SDK-based approach. No actively-maintained,
  broadly-adopted async Solr client for Rust was available with confidence
  comparable to `meilisearch-sdk`'s; hand-rolling the (well-documented,
  stable) Core Admin/Schema/Update/`/select` APIs directly was the more
  reliable choice given no live Solr instance was available to validate
  against during development.
- **Solr's JSON Request API** (`/query` with a structured JSON body,
  available since Solr 7) instead of the classic `q`/`fq` parameters.
  The JSON Request API is closer in spirit to Elasticsearch's Query DSL,
  but the classic parser is more universally supported across Solr
  versions and deployments, and - critically - is expressive enough on
  its own (via Lucene boolean syntax) to represent the entire `Query` DSL
  without needing the JSON API's structure at all.
- **SolrCloud's Collections API** instead of standalone Solr's Core Admin
  API, for parity with production Solr deployments (which are usually
  cloud-mode). Deferred: Collections API requires a ZooKeeper-backed
  cluster and a pre-uploaded configset, meaningfully more setup than a
  single Solr instance, and out of scope for getting a first working
  backend in place. Tracked as a known limitation rather than silently
  assumed away.

## Consequences
- `rusty-search-solr` duplicates *conceptually* similar work to
  `rusty-search-elasticsearch` (schema translation, query translation,
  a local index/field-type registry, error mapping) without sharing any
  code with it - a direct consequence of Solr's API not being wire-compatible
  the way OpenSearch's is. This is the correct call given the actual API
  shapes, not an inconsistency with ADR-0004: that ADR wraps because the
  protocols match; this one doesn't, because they don't.
- This backend is, ADR-for-ADR, the most expressive one in the workspace:
  it can represent `Query` trees that `rusty-search-meilisearch` rejects.
  Application code that wants to run identically across every backend
  should still write to the *intersection* of what all backends support
  (documented per-backend), not assume Solr's extra expressiveness is
  portable.
- No automated test in this repo exercises a real Solr instance
  end-to-end - the same disclosed gap as every other remote backend in
  this workspace (ADR-0002, ADR-0003). The defensive error-body parsing
  in particular is a judgment call made without a live server to confirm
  Solr's actual status-code behavior against; it's a reasonable, safe
  default (checking the body costs nothing when the status code is
  already correct) rather than a verified fact about every Solr version.
- Only standalone Solr (Core Admin API, `_default` configset) is
  supported; SolrCloud is not, until the Collections API is added as a
  follow-up.
