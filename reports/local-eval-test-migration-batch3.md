# Evaluation test migration, batch 3

Authored and formatted; root's serialized compilation/test verification remains pending. No live provider, database, GUI or sandbox execution occurred in this lane.

## RAG gate source suite

`tests/services/eval/test_rag_regression_gate.py` maps to:

- `platform-eval/tests/eval_migration_parity_rag.rs`: frozen 12-case/two-track assertions, all retrieval metrics/CI, explicit answer-not-run scope, critical metadata type, 200-case cohort with one critical retrieval miss, empty/undersized/low-quality gates, order/seed determinism, all three cross-track evidence contamination cases, and invalid statistical configuration.
- `platform-eval/tests/eval_migration_parity_judges.rs`: 15 isolated environment-pin matrix modes plus CLI guards/report tests. Actual policy resolver and grader cover explicit pin, wrong pin, four semantic policy mutations despite matching raw pin, passing bound cohort, mixed/wrong cohort, changed evaluated answer, provenance mismatch, missing/low evidence, artifact changes after policy pin, unrounded 0.69996 score, and one critical answer failure within 200 otherwise strong cases. Passing mode also checks actual CLI raw-byte judge snapshot/provenance. External provider execution remains not_run.

The pin is set only on isolated child test processes. No global environment mutation occurs across concurrent tests. Dynamic Python wrong-type numeric/list inputs map to native typed JSON rejection. Native `Policy` private fields enforce trusted resolved-policy construction; CLI missing-policy/opt-in gates are exercised rather than constructing untrusted policy objects.

All 22 named Python test groups and their parameter variants now have retained behavioral assertions. Whole-file retirement is eligible only after actual native verification.

## Stateful source suite

`tests/services/eval/test_stateful_agent_eval.py` maps to
`platform-eval/tests/eval_migration_parity_stateful.rs` (17 consolidated tests).

Retains all 24 named source groups and parameter variants: seven passing frozen contracts; fourteen regression mutations; nine missing expectation fields; unknown fields; malformed turn/plan evidence; terminal budget exhaustion; compaction drop accounting; call/result ordering; nine terminal statuses and five missing/nonterminal statuses; post-approval identity/argument substitution; five nondispatch result statuses; three unapproved/protected-call attacks; unrelated safe read; six malformed HITL receipts and three invalid resume counts; generic tool-pairing composition; duplicate result order and terminal conflicts; top-level provenance precedence; and absent/wrong compaction lineage.

Assertions call real native `golden::evaluate_case` / `golden_validation::validate_cases` and preserve source failure categories/messages. No synthetic substitute evaluator is used. Whole-file retirement awaits root verification.
