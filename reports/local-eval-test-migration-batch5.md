# Eval test migration batch 5

Target `ai-platform-gateway --test eval_migration_parity_scoring` currently has 12 tests against actual production rule/trajectory/span/composite/LLM scorer APIs. Source: `tests/services/eval/test_evaluator_executor.py`.

Covered scoring assertions: runnable reviewed/promoted dataset selection; required/missing span kinds and metadata; workflow/metric trajectory summary; retrieval document count; error spans; embedded nested JSON response; empty/unknown/malformed rules including zero and partial thresholds; two per-span target identities and nested rules config; composite components; valid/unknown-label/malformed LLM response scoring; exact reference-JSON prompt text and trajectory; three truncation markers with <=14,000-character prompt limit. LLM calls use only isolated loopback fixture responses and assert one request per score.

These tests intentionally do not assert database run lifecycle, score persistence counts, target resolution/completeness, frozen manifests, RAGAS retries or orchestrator summaries. Those source assertions remain outstanding. The source whole file is not retirement eligible. Python callback TypeError introspection is language-specific, but no duplicate provider attempt on transport/scoring failure still needs actual behavioral coverage.

Formatted, not executed by this lane. Parent serialized verification pending; no Cargo/manifests/dependency edits or live provider calls.

Scorer target expanded to 17 tests with real loopback RAGAS adapter composition: two metric payloads and local threshold override; retrieval-only answer metrics remain semantic review without provider dispatch; canonical answer/response-relevancy alias; missing sample/span versus missing client classification; missing/duplicate/mixed/empty/out-of-range service results produce infrastructure review for each expected metric. Persistence/orchestrator assertions remain outside this target.
