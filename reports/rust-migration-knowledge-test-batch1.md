# Knowledge test migration batch 1

Checkpoint: written/formatted, awaiting root compilation and pure execution. No
Python file is authorized for retirement merely by this report.

Native targets (package ai-platform-knowledge-service):
- knowledge_migration_parity_parsing: 11 tests
- knowledge_migration_parity_config: 5 tests
- knowledge_migration_parity_tools: 2 tests

## Source suite disposition

1. `tests/services/knowledge/test_parsing_ir.py`: all original assertion groups now
   represented: exact deterministic serde roundtrip, schema version preservation,
   unknown-kind sparse IR, reading order/page lookup, stats, heading/table/formula/
   figure rendering, HTML-only summary, page markers, citation/payload, global
   payload order and BBox guards. Consolidated into five native tests. Eligible
   for source-test retirement review only after all tests pass.
2. `test_parsing_table_policy.py`: all original assertion groups represented:
   20/21/100/101 boundaries, markdown header parsing, unchanged small/HTML-only
   tables, 50-row segmented coverage/header/overlap/provenance/unique IDs, 120-row
   level output, pure document rewrite with unchanged small table HTML. Three
   native tests. Eligible after passing.
3. `test_parsing_backends.py`: payload conversions for MinerU/Paddle and text
   paragraph/confidence are represented; registry protocol introspection,
   injected text extractor, all OCR/VLM injection/availability branches and
   standalone markdown structure cases remain. Not file-retirement eligible.
4. `test_parsing_cascade.py`: first acceptable stage, unavailable stage skip,
   low-confidence best/hard page, backend exception fallback, all failed, cache
   hit/copy, backend-version key difference, default config roundtrip, empty
   config and confidence semantics covered. Predicate-specific cases, complete
   document ordering/partial/resume, options, unknown build-time backend and
   all source version assertions remain. Not file-retirement eligible.
5. `test_document_metadata_registry.py`: reserved/duplicate/immutable fields and
   typed timezone-normalized bounded patch cases covered. NaN is not a JSON
   number and Rust rejects unsupported typed values; source NaN-specific error
   string not claimed. DB schema CAS, patch preservation/bulk mutation cases
   remain. Not file-retirement eligible.
6. `test_chunking_config_schema_roundtrip.py`: four unsafe runtime field rejection
   cases covered through actual production validator. API lossless raw DTO
   roundtrip and archive-reason schema width remain. Not file-retirement eligible.
7. `tests/test_chunking_config_chain.py`: the two meaningful assertion groups are
   now against real ChunkingConfig rather than a copied Python simulation:
   400-token inferred limits and explicit min/max/parent/child values. Remaining
   source functions only print documentation/log examples. Eligible for review
   after passing, with simulation-vs-runtime distinction recorded.
8. `test_text_reranker_factory.py`: BGE provider inference, DashScope model alias,
   regional workspace/shared Singapore endpoints and absent-region refusal
   covered. Provider transport/cache/circuit and model concurrency cases remain.
9. `test_langgraph_tool_security.py`: real public constructors/schema bounds,
   invalid execution through retriever/single/multi/Dify entrypoints assert the
   precise Validation error variant before a closed DB pool can be reached.
   NaN/Inf are not representable JSON numbers; typed usize constructor cannot
   accept bool/string. Successful regular-text retrieval boundaries and the
   LangChain descriptor schema variant remain. Not file-retirement eligible.
10. `test_token_window_unicode.py`: existing shared integrations chunking tests
    already cover exact shared fixture, real Unicode document boundary, terminal
    overlap and small-budget cross-product; no duplicate new tests. Verification
    must use that existing target; this batch does not claim to rerun it.

## Minimal production parity corrections

Authoritative archived source `parsing/ir.py` and `parsing/table_policy.py` showed
missing native sparse-IR defaults, unknown type fallback, BBox convenience,
ordered page/block helpers/stats, payload iterator and pure table rewrite API.
These were added to shared `parser.rs`/`structured.rs` with parent approval and
Gateway-owner coordination. Table split reuses the new extracted header/tier
helpers; no default cascade activation, external endpoint or persistence change.

## Verification correction after root run 75

Root observed the config-chain test fail at expected1200/actual1600. Inspection
of archived production `chunking.py:1159` confirms `from_dict` uses
`max(child_token_limit * 4, 900)`. The original standalone test defines its own
stale fake `from_dict` using `token_limit * 3`; only dataclass `__post_init__`
uses the latter fallback. Native `from_value` correctly implements the real
production parser. The native test now asserts1600 with this evidence, without
changing production. Retract any claim that the standalone simulated1200
assertion was migrated; that original suite is not whole-file retirement
eligible on this test alone. Explicit supplied budgets remain covered.
