# Knowledge test migration batch 2

Written/formatted; root owns compilation/execution. No live DB, Qdrant, weights,
or external provider calls were made. The reranker tests use an isolated loopback
HTTP fixture and the actual reqwest adapter. Earlier ignored DB tests stay opt-in.

## Native targets

- knowledge_migration_parity_eval: 6 tests
- knowledge_migration_parity_retrieval: 10 tests
- knowledge_migration_parity_assets: 4 tests
- knowledge_migration_parity_rerank: 3 tests

Together with batch 1, seven integration targets contain 41 tests.

## Exact source assertions / retirement status

1. `test_parsing_eval.py`: all six original cases implemented, with all original
   numerical assertions: Unicode edit distance, identical/degraded/different
   table grids, LaTeX normalization, matched reading-order reversal/swap/subset,
   perfect document and bad parse penalty. Retirement review eligible after pass.
2. `test_t9_parent_child_unit.py`: disabled/default/explicit/invalid parent-child,
   summary and structural settings; three fanout leg widening examples; max-score
   summary merge, stronger-summary score, prefix opt-out, unresolved passthrough,
   source union; affine-only structural boost, clamp, empty query. Pure cases
   consolidated into retrieval tests. Parent DB folding/cancellation/wrong-ID,
   breadcrumb standalone API and exact fanout-report cases remain.
3. `test_t9_parent_child_pipeline.py`: summary max-score/body preservation and
   structural affine boost mechanism asserted through real functions. Whole
   pipeline ordering, top-k-before/after-fold, context mode, unresolved parent and
   other DB-backed pipeline cases remain. Not eligible as a whole suite.
4. `test_dataset_revision_fingerprint.py`: real production fingerprint changes
   for revision/effective retrieval config/nested RRF weights/lexical profile,
   excludes rerank/embedding key values and absent revision emits no fingerprint.
   Catalog wrapper rows/statistics equality and successful reindex revision DB
   increment remain. Not eligible as a whole suite.
5. `test_document_asset_storage_scope.py`: exact trimmed owned-prefix deletion,
   aggregate count, all five invalid cleanup scopes and all seven invalid image
   keys with zero backend calls, valid scoped download and absent signing key
   refusal. Recording backend implements real ObjectStorage boundary. Signed URL
   verifier equivalence case remains. Not yet whole-file eligible.
6. `test_rerank_adapter_defensive_resort.py`: all three original cases represented
   through actual HTTP adapter: legacy unsorted output, flat unsorted output,
   sorted cache replay. Also verifies only one provider request for cache hit.
   Retirement review eligible after pass.
7. `test_text_reranker_factory.py`: additional real request-schema assertions,
   top_n-isolated cache and delimiter-safe document boundaries. Previous pure
   factory/region assertions in batch 1 retained. Full provider-circuit, loading
   concurrency and remaining factory cases still pending.
8. `test_public_kb_security_contracts.py`: actual public/persisted retrieval
   resolver rejects seven over-limit knobs plus query >4096; this is the same
   resource contract below HTTP schema. Whole HTTP schema, batch, QA paid-budget,
   preview and text-document bounds remain distinct obligations.
9. `test_relevance_contracts.py`: negative and >1 request thresholds rejected
   through production resolver; remaining dense-only threshold application,
   pre-recall behavior, hybrid union, rerank pipeline and LIKE escaping remain.
10. `test_bm25_v2_shadow.py`: legacy inactive/shadow defaults, full schema
    fingerprint, metadata field/model/tokenizer/language/index profile roundtrip,
    changed k, active-without-shadow refusal and unknown option refusal covered.
    Live/native vector mutation, capability, receipts, rollback and publication
    compensation assertions remain. Not whole-file eligible.

## Source-proven production corrections

- Added lightweight Knowledge `parsing_eval.rs` implementing the archived
  `parsing/eval.py` pure metrics. No runtime dependency on the large eval crate,
  no corpus fabrication, and TEDS-lite explicitly stays cell-grid comparison.
- Corrected AssetStorage deletion from the whole documents subtree to the exact
  source-owned images and original-file prefixes; trims scope as source does,
  returns backend deletion count, and adds exact-scoped image download helper.
  Recorded source tests demonstrate this boundary. Parent adds a semicolon to
  the existing service.rs tail call that intentionally discards the new count;
  all other production call sites already discard it. No permissions expanded.
