# Retrieval v2 implementation (U-KN-09)

Quality options live in `dataset.index_config.retrieval.v2`. Omission/false preserves the
existing pipeline. The typed profile rejects unknown fields and provider endpoint/key overrides.
No option is promoted by this implementation or by its synthetic tests.

```json
{"version":2,"profile_id":"reviewed-profile-version","query":{"normalize":false,"detect_language":false,"language_weights":{},"conversational_rewrite":false,"multi_query":false,"synonyms":{}},"cascade":null,"contextual_headers":false,"deduplicate":false}
```

Normalization uses NFKC, Unicode case folding and whitespace collapse. Detection reports a
conservative script language; optional reviewed `language_weights` maps a language to
`[dense_weight, bm25_weight]`, without adding a recall filter. Rewriting accepts bounded
`conversation: [{"role":"user","content":"previous question"}]` request extras and uses only
the last user question as context. Multi-query uses at most two reviewed alternatives for a
normalized short-query key, plus identifier punctuation expansion within a three-query cap.
Every generated query retains the original document/metadata/language/source filters.
Hierarchical retrieval rejects enabled v2 options explicitly; its existing protocol is preserved.

A cascade has `budget_ms` (50–2500) and one or two pinned
`{provider, model, revision, candidates}` stages. The first stage is `bge`/`local`, CPU PairScores,
up to 50 candidates. The optional second registered reranker handles up to 10 of that ranked
prefix. Both reserve/execute through the existing model plane with actor/resource scope,
revision pinning, receipts and the smaller of the profile and interactive remaining budgets.
Malformed/partial scores, unavailable scope, error or timeout preserve the last completed ranking;
there is no ungoverned fallback and degraded results are not cached.

Contextual embedding headers contain bounded title, section path, document context and available
caption/description. Raw display/citation text and lexical input remain unchanged. Header selection participates in
ingestion identity, so existing documents require a reindexed generation; manual segment embedding
uses the same header contract. Existing DocIR
v2 table/formula atomicity and section paths are reused. Ranked-pool MinHash deduplication removes
near-identical cross-document/version text within the already authorized recall set. It does not
delete stored data or share candidates between ACL scopes.

Region citations come from active SQL chunk references joined to the immutable DocIR execution
manifest and document version. Vector payloads cannot supply version authority. Results carry the
existing wire type plus `metadata.citation` (DocIR v2 Citation), version and page metadata. A missing
region remains null; no geometry is invented. Manually overridden segment bodies do not inherit an
immutable region citation. Existing region-fetch authorization and generation
checks remain in force. QA context includes the corresponding document/version/page/block label.

## Promotion and evidence

`RetrievalService::new` and `Indexer::new` read the server-only
`KNOWLEDGE_RETRIEVAL_V2_PROMOTIONS_FILE`, a bounded JSON decision produced by the native
`knowledge-eval-gate`. Missing file configuration means no quality permits. An invalid configured
file fails closed for v2 requests. Dataset booleans and request extras cannot grant permits.

The existing gate retains corpus, hash, evaluator, source, per-case nonregression and calibrated
judge requirements. Its additional `retrieval_v2_promotions` field binds each exact canonical typed
profile plus exact tenant/dataset/model/index configuration fingerprint to independently measured positive mean nDCG@10 gains for **all** enabled
options. Captures use a full dataset/model configuration artifact with `index_config.retrieval.v2`.
Root-only index configurations can be measured but cannot grant a serving permit. Content
generations may advance; a changed implementation, model revision, dataset scope or executable index config
requires a new decision. The implementation hash covers the executed query/header/cascade paths.
Include same-suite leave-one-option-out
controls whose other executable settings match. Missing controls, no gain or any per-case
regression withholds that profile's permit. Names may differ between ablations; runtime fingerprints
still bind the full profile including its name, dataset scope and model/index configuration. Fixture decisions always have an empty permit map.

Unit tests are synthetic contract evidence. Real corpus nDCG gains, provider receipts, SQL/Qdrant
publication races, cross-ACL integration and p95 latency acceptance are **not_run** here. The
multimodal refusal is retained. Activation, model registration and reindexing remain separate
operator actions after real evaluation; no downloads or business service startup occur here.
