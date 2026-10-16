# Knowledge test migration batch 3

New independent targets only; previously active files remained unchanged during
root's compile retry. Formatted, not executed by this worker. Root owns Cargo.

- `knowledge_migration_parity_multilingual`: 6 tests
- `knowledge_migration_parity_mmr`: 7 tests
- `knowledge_migration_parity_query_settings`: 7 tests

Total across the ten new batch 1–3 integration targets: 61 tests.

## Source mappings and retirement eligibility

1. `tests/knowledge/test_knowledge_improvements.py`: all six original test
   functions and their assertions are represented by the multilingual target.
   Arabic-only input exercises the production multilingual tokenizer's Arabic
   branch. Token chunking uses the same sample and all three limits. Candidate
   for retirement only after root verifies the target passes.
2. `tests/services/knowledge/test_b5_mmr_fill_policy.py`: strict/fill rejection,
   absent pick annotations on filled tails, no-threshold policy parity, empty
   inputs, lambda extremes, missing/zero vectors, and brute-force equivalence
   including score/relevance/max-sim annotations are represented. The reference
   vectors use a deterministic native generator with the same three seeds,
   24 vectors, eight dimensions, near-duplicate pair and four lambdas; they are
   not byte-identical to Python Random's vectors. Resolver default policy,
   shorthand, explicit precedence, all four bad policies and nonboolean
   strict_diversity are asserted through production functions. Actual DB
   pipeline tenant/vector scoping, shortfall metadata and result payload fields
   remain; whole source suite is not yet eligible.
3. `test_t2_8_query_preset_flags.py`: absent/disabled forms, exact shorthand echo,
   normalization, all rewrite presets, unknown presets, HyDE no-preset rule,
   four malformed shapes, and scalar/object fingerprint variation represented
   by query-settings tests. Read resolver rejection is real. Dataset write
   wrapper and actual successful pipeline inertness/meta assertions remain.
4. `test_retrieval_fusion_validation.py`: legacy unweighted default; all three
   explicit weighted-RRF opt-ins; both nested alias maps; weighted alpha; five
   representable invalid persisted configs are asserted through production
   resolver. Literal non-finite wire JSON is rejected by serde, with no false
   NaN-to-null assertion. Full single/batch HTTP schema parity and in-memory
   Python non-finite object behavior are not claimed. Whole suite remains.
5. `test_token_window_unicode.py`: existing native chunking unit tests already
   implement the shared JSON corpus, exact source-substring coverage/budgets,
   realistic pipeline boundary, terminal overlap and all 108 small-budget
   combinations. No duplicate tests added. Native strict windows share the same
   `fixed_windows` implementation. Root must attach existing verification
   evidence before retirement; this batch did not execute these tests.

## Display-status follow-on

`knowledge_migration_parity_display` adds six tests consolidating all pure
projection groups in `test_document_display_status.py`: exact lifecycle mapping,
archive precedence, enabled defaults, unknown/nonobject inputs, normalization,
closed-vocabulary cross product, identity preservation, secret redaction and
safe specialized-rebuild guidance. The original DB-backed viewer/editor
active-document assertion remains; no whole-suite retirement claim.

The new tests exposed two source-proven minimal parity repairs. Projection now
passes nonobjects through instead of panicking/coercing null. Error projection
restores only the archived allowlisted specialized failure code and its fixed
safe instruction, plus the fixed generic retry instruction. All newly added
advisory processing diagnostics remain intact; raw errors are never returned.
Parent authorized this repair. Written/formatted, awaiting root verification.
Total new batch 1–3 targets including this follow-on: 67 tests in 11 targets.
