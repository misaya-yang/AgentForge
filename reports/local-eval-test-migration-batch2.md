# Evaluation test migration, batch 2

Authored/formatted; all execution remains with root's serialized verifier. No provider, GUI, or database operations were run by this lane.

## Complete behavioral source mappings, verification pending

- `tests/services/eval/test_retrieval_metrics.py` → platform-eval `eval_migration_parity_retrieval` (11 tests). All hand-computed hit/precision/recall/RR/AP/binary+graded NDCG, aggregation, empty report, duplicate-slot credit, per-cutoff truncation, invalid grades/cutoffs and duplicate query ID assertions retained. Typed `usize`/JSON rejection replaces Python bool/float/negative cutoff acceptance into a dynamic function. The native JSON report uses largest cutoff as the primary record rather than Python object identity.
- `test_assistant_capability_contract.py` → platform-eval `eval_migration_parity_capability` (8 tests, both proxy-name cases). All quality/critical repetition, substantive outcome/safety, mixed tier, exact trial indices, 90% case threshold, and invalid critical-policy assertions retained.
- `test_agent_runtime_eval_contract.py` → existing platform-eval `runtime_contract_source` (7 named groups, including bad sequence variants). Existing exact native port verified by inspection; no redundant new implementation or passing claim.
- `test_rerank_bakeoff_cases.py` → knowledge-ops test-only `bakeoff::eval_migration_parity_bakeoff` child module. Fixture language/cross-language counts, grade-three inclusion, Chinese headroom, oracle metric/promotion, and byte-exact builder assertions retained. Oracle exercises real private production metrics and gate; the test does not claim provider adapter orchestration.

## Partial source mappings; retain Python references

- `test_rerank_bakeoff.py`: knowledge-ops child module additionally preserves loader rejection and zero-grade removal, hand-computed NDCG/stable ties, strict Chinese improvement and English non-regression, ineligible challenger rejection, missing baseline/Chinese slice, actual identity-only offline CLI/report output, unknown provider, missing identity, and mandatory live flag refusal. Residual: injected adapter score-length/error aggregation, reranker top-N/padding and complete challenger runner/report integration, provider default model/URL and missing-key cases. These are not replaced by helper-only coverage.
- `test_coding_parallel_fixture.py` → platform-eval `eval_migration_parity_coding` (6 tests) covers native-v2 scenario identities and immutable sources, unchanged repetitions/parallel/line/overlap thresholds, patch scope/self-report denial, real timeline ordering/overlap, bare-string text deltas, strict duplicate-key/artifact parsing, and offline receipt ineligibility. Residual: complete independent collector/host/golden HMAC live-chain mutation matrix and restricted sandbox reference-patch/unsolved-patch execution. No synthetic observation is declared live.
- `test_runtime_model_evidence.py` → gateway `eval_migration_parity_model_evidence` (5 tests) covers actual durable projection, six scope/model mutations, tool-effect projection without arguments, unobserved-call/unknown-usage refusal, and persisted fingerprint-only projection. Residual: repository scoped SQL, score UUID row decode, terminal-candidate reconciliation without redispatch and version mismatch, and worker orchestration cases. No fake pure projection is substituted for repository behavior.

## Coding fixture test-code counterpart

All five first-party tests under coding-v1 repository/tests have exact native-v2 repository/tests counterparts:
- allocation: identical two cent totals, tie-break IDs and largest remainder outputs;
- idempotency: same nested-object reorder equivalence and tenant inequality;
- service: same reordered retry, same instance (`Arc::ptr_eq`), one ledger row, allocation and cent balance.

The fixture intentionally includes the original business defects. Source translation does not mean these five tests should all pass on the unsolved fixture. Reference-patch run plus expected unsolved failures are still required before claiming functional benchmark verification. New native-v2 provenance, dependencies, immutable hashes and image identities remain distinct from historical v1 receipts.
