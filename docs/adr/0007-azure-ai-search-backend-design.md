# ADR-0007: Azure AI Search backend design

Status: Accepted
Date: 2026-07-21

## Context
[Azure AI Search](https://azure.microsoft.com/en-us/products/ai-services/ai-search)
(formerly Azure Cognitive Search) is another hosted search service, like
Algolia - there's no cluster to point at, only a *service* (identified by
an endpoint URL) containing indexes, authenticated with an `api-key`
header. As with every prior remote backend, adding
`rusty-search-azure-search` raises the same two questions: hand-roll HTTP
or lean on an existing crate, and how much of the `Query` DSL can this
backend's native query model represent?

No actively-maintained, broadly-adopted async Azure AI Search Rust client
exists on crates.io with confidence comparable to `meilisearch-sdk`'s (the
official `azure_search_documents` crate that exists in some Azure SDK
ecosystems is a Track 1/preview-era package with unclear maintenance
status in the Rust ecosystem specifically) - so, as with Solr and Algolia,
this backend hand-rolls the REST API over `reqwest`.

Azure's REST API has a genuinely distinguishing property none of the
other backends in this workspace share: its search parameter, `search`,
can be sent with `queryType: "full"` to opt into full Lucene classic query
syntax - the *same* query language Solr speaks, field-scoped clauses and
arbitrary boolean nesting included - while *separately* exposing a real
OData `$filter` parameter, a genuinely distinct, non-Lucene grammar, for
non-scoring filter clauses. No other backend in this workspace has two
independent, fully-fledged query languages available in one request; this
shapes the entire query-translation design below.

## Decision
- `rusty-search-azure-search` is hand-rolled over `reqwest`, matching the
  Elasticsearch/Solr/Algolia approach.
- The index's key field is always named `"id"` (not configurable) and
  mapped as `Edm.String`, `key: true`. Azure restricts key values to
  letters, digits, underscore, dash, and equal sign; this isn't validated
  client-side, matching this workspace's general stance of surfacing the
  backend's own rejection rather than pre-validating every constraint.
- `FieldOptions::fast` maps onto Azure's `sortable` field attribute. This
  is the first remote backend in this workspace where `fast` does
  anything: Elasticsearch already makes every sortable core type sortable
  by default (ADR-0002), so `fast` is a no-op there, but Azure genuinely
  requires `sortable: true` to be declared at index-creation time before a
  field can be used in a native `$orderby` clause - the same shape
  `rusty-search-tantivy`'s "fast fields" already have in this workspace,
  just surfacing in a *remote* backend for the first time. A
  `SearchRequest` sorting by a non-sortable field falls back to the same
  `FALLBACK_SORT_CAP`-bounded in-memory sort Tantivy/Algolia already use.
- Writes are synchronous HTTP calls with no task to poll - unlike
  Meilisearch/Algolia. `commit()` is still a no-op here, but for a
  different reason than either of those: Azure has no refresh/commit
  concept at all (indexing is automatically near-real-time), so there is
  nothing to trigger, rather than "already triggered internally by the
  time the write returned."
- `Query` translation splits across Azure's two independent grammars:
  - `must`/`should`/`must_not` translate into a single full-Lucene-syntax
    `search` string, using the *same* grounding trick ADR-0005 established
    for Solr (`* AND (<tree>)`, using Azure's documented `search=*`
    "match everything" token as the outermost left operand) - because
    Azure's full Lucene syntax supports arbitrary nesting and field-scoped
    clauses exactly like Solr's, more than one `Query::Match` is
    representable here too, unlike Meilisearch/Algolia's one-clause
    ceiling.
  - `Query::Bool::filter` children translate into a separate OData
    `$filter` expression - a distinct, non-Lucene grammar with no
    full-text primitive, so `Query::Match` nested inside `filter` is
    rejected with `SearchError::InvalidQuery` (use `must`/`should`/
    `must_not` instead, which always route through `search`).
  - `must_not` wrapping a bare `Query::MatchAll` is accepted inside
    `Query::Bool::filter` (OData has real `true`/`false` boolean literals,
    usable anywhere in its grammar, unlike Algolia's filter language) but
    rejected when it appears among `must`/`should`/`must_not` at the
    `search`-string level: Azure's bare `*` is only documented as valid in
    the single outermost position this module already grounds with, and
    nesting it inside a parenthesized sub-expression is not a behavior
    this crate is confident enough in to rely on without a live service to
    verify against.

## Alternatives considered
- **Depend on an existing Azure Search Rust client crate.** Rejected for
  the same reason as Solr/Algolia: nothing met the bar
  `meilisearch-sdk` set for trustworthiness/adoption/maintenance in this
  specific ecosystem.
- **Use Azure's "simple" query syntax instead of `queryType: "full"`.**
  Rejected: simple syntax doesn't support the field-scoped, arbitrarily
  nested boolean expressions this crate's `Query::Bool` needs to translate
  losslessly; full Lucene syntax is the only mode expressive enough to
  match Solr's level of completeness.
- **Attempt the `*:*`-style grounding trick for `must_not(MatchAll)`
  anywhere in the `search` tree, not just at the outermost position**,
  for full parity with Solr. Rejected: unlike Solr's `*:*`, which is a
  genuine Lucene clause (a wildcard match against a real field) documented
  as valid in any position, Azure's bare `*` is a service-level shortcut
  specifically documented for the *entire* search value; embedding it
  mid-expression is unverified behavior with no live service available to
  confirm it. Rejecting this narrow case is more honest than an
  unverifiable claim of full completeness (the same "honest limitation
  over fragile approximation" philosophy ADR-0003/ADR-0006 already
  established).
- **Skip the OData `$filter` split entirely and fold everything into the
  `search` string**, since Lucene syntax alone is already expressive
  enough to represent the whole `Query` tree. Rejected: `Query::Bool::filter`
  exists specifically to request a genuinely non-scoring clause (matching
  Elasticsearch's real `filter` context and Solr's `fq`); folding it into
  `search` would make every filter clause contribute to relevance scoring,
  changing result ordering in a way the core `Query` type's own
  documentation says `filter` shouldn't.
- **Ignore Azure's `sortable` requirement and always use the in-memory
  fallback sort.** Rejected: `rusty-search-tantivy` already proves a
  native/fallback split is worth the extra `FieldMeta` bookkeeping when
  the underlying engine genuinely has a fast/non-fast field distinction;
  treating every Azure field as non-sortable would silently discard a
  real capability the schema already declared via `FieldOptions::fast`.

## Consequences
- `rusty-search-azure-search`'s `search`-string translation logic closely
  parallels `rusty-search-solr`'s (both speak Lucene classic syntax), but
  is not code-shared with it, since the two backends' surrounding REST
  shapes (JSON document/batch format, index definition schema, auth
  headers, endpoint conventions) are entirely different - the same
  "conceptually similar, not actually shared" relationship ADR-0005
  already establishes between Solr and Elasticsearch.
  - This is also the first backend in this workspace whose completeness
    depends on *which* of two sub-languages a clause routes through:
    full Solr-level expressiveness (multiple `Query::Match`, `must_not`
    wrapping `Query::Match`) at the `search`/Lucene level, but a narrower
    boundary than Solr for `must_not(Query::MatchAll)` specifically, and
    for `Query::Match` inside `Query::Bool::filter`.
- Applications that rely on Azure Active Directory/managed-identity auth,
  or that need every field sortable without explicitly opting each one
  into `FieldOptions::fast`, won't get that through this backend as-is.
- No automated test in this repo exercises a real Azure AI Search
  service - the same disclosed gap as every other remote backend in this
  workspace (ADR-0002, ADR-0003, ADR-0005, ADR-0006). The
  `must_not(MatchAll)`-at-the-search-level restriction in particular is a
  judgment call made without a live service to confirm Azure's exact
  Lucene-parser behavior against; it's the conservative, disclosed choice
  rather than an unverified claim of full completeness.
