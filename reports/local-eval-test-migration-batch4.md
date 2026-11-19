# Local/Eval test migration batch 4

Native target: `ai-platform-eval --test eval_migration_parity_producer`.
Source: `tests/services/eval/test_agent_observation_adapter.py`.
Status: authored and rustfmt parsed; Cargo verification belongs to the serialized parent queue. No live services were called.

The 13 consolidated tests retain the source business assertions for canonical tools and durable HITL, checkpoint identity, monotonic budget projection, negative arithmetic and all invalid bound variants, exhaustion event arithmetic, unknown unrecorded plans, trace-only evidence rejection, cross-run joining, process-only checkpoint rejection, three pre-dispatch checkpoint mutations, two injected runtime-control arguments, approval result ordering, trace/terminal status conflict, tool name aliases and missing identity, terminal presence/version/exit mapping, missing tool name/success, cross-turn lifecycle, conflicting success, verified failed tool receipts failing the business gate, bootstrap-only/nonmonotonic/nonexhausted budget rejection, and both recorded-candidate CLI paths.

Inputs reuse the frozen original-source fixture `src/producer_goldens.json`. Assertions run production native adapter, golden evaluator, and candidate CLI. Nested Python shared-reference terminal mutations are applied to both JSON receipt copies. Python NaN is rejected at the native JSON parser boundary; a null invalid numeric value additionally exercises adapter rejection. There is no interpreter or Python runtime import.

Retirement eligibility: this source file's behavioral cases are represented, pending passing native target verification. Python importlib entrypoint loading is replaced by the native exported CLI entrypoint. Live runtime/provider evidence remains `not_run`; recorded fixture receipts cannot claim live execution.

Related actual verification: `local-eval-parity77.log` confirms acceptance24, capability8, coding6, exact-output4, golden23 passed. Golden CLI had 3 passes and one deterministic component-order failure. Production provenance now sorts component keys explicitly, preserving the source sorted-list contract and digest checks; rerun is pending. Later targets were not executed after that failure.

## Experiment statistics

Additional target: `ai-platform-gateway --test eval_migration_parity_statistics`, nine tests, all behavioral assertions from `tests/services/eval/test_experiment_statistics.py`.

Exercises production `eval_comparison::{aggregate_live_case_rows,compare_runs}` for missing trial usage, execution-success/judge-failure separation, critical/quality/execution regressions, unscored execution faults, two fixed version identity comparisons and shared critical failure, unconstrained efficiency warnings, explicit performance and manifest incompatibility, and forged versioned metrics. The Python test repository overrides its fetch methods to supply rows; native production repository delegates comparison to precisely these tested pure functions. No SQL acquisition assertion was present in the source file. Retirement eligibility is pending a passing native target run. Formatted, not executed here.

## Dataset manifest identity (partial)

`ai-platform-gateway --test eval_migration_parity_manifest` retains the two pure source groups in `test_dataset_manifest_identity.py`, including all five behavior/review/provenance mutation variants, SQL JSON normalization, and revision binding. The source integration case comparing hashes in release/live enqueue/rescore writes is still outstanding. Do not retire this entire source file on the strength of these two tests. Native target is formatted and awaiting parent execution.

## Trace capture helpers (partial)

`ai-platform-gateway --test eval_migration_parity_trace_capture` adds six tests covering source pure retrieval-path checks, LangGraph identity extraction and five-route matrix, lifecycle parenting, RAG trace fields, request/response/document parsing, and SSE backend trace retention. Both short and full Knowledge paths preserve dataset identity and bounded retrieval documents through the actual production composition functions.

Residual: source background scheduling, disabled-database behavior, and actual record-to-ingest enqueue flag assertions are not established by pure payload tests. Do not retire `test_trace_capture_helpers.py` in full until the scheduling path has native execution coverage. No production seams or dependencies added. Target formatted, execution pending.

## Fixed finance fixture

`ai-platform-eval --test eval_migration_parity_finance`: seven consolidated tests retain all fourteen source groups in `test_real_finance_salesforce_eval.py`. Actual native fixture validation checks 25 formulas/hashes offline, scenario contract preserves three parallel runs and host-only numeric goldens, CLI grading applies minimum repetition score and unique run identity, and deterministic scoring retains misleading annualization, non-GAAP, unit scale, source whitelist, interval overlap, packet binding, and final-answer hash caps. Duplicate JSON keys are rejected by the production parser. Synthetic passing receipts prove grading behavior only; they are not live provider evidence. Target formatted, parent execution pending.

## CRA deterministic fixture (partial, with exact production repair)

`ai-platform-eval --test eval_migration_parity_cra`: nine tests retain the first ten deterministic collector/scorer source groups (two malformed-answer groups consolidated). Covers independent official source integrity, hidden packet fields, judge eligibility without task success, stale proposal and dates, canary/missing/adverse evidence, exact answer shape, required actions, independently pinned provenance manifest, rehashed snapshot tampering, and rejection of a nonofficial remote URL before any network call.

The rehashed-snapshot source assertion exposed a real direct-evaluator validation gap. With parent approval, `cra::evaluate_candidate` now validates the fixture against embedded frozen provenance before scoring. `pinned_provenance` is shared with file loading and uses the one existing code-owned PIN; no goldens or thresholds changed. The CLI already validated at load time, but direct callers now retain the source guard too.

Residual: four source real-agent scenario/hidden-gold/reasoning validation groups still need full source-equivalent matrices. Existing production `scenario` functions are available; no live run is required for their pure assertions. This source is not yet eligible for wholesale retirement. All new tests and repair formatted; actual execution remains in parent queue.

CRA follow-on completed: four additional tests now retain the remaining source scenario groups, for a total of 13 native tests covering all 14 source groups. The manually translated source reference answer is tested against actual scenario assertions, then mutated for stale precedence/dates, unsafe attachment/uncertainty, unknown fields and missing reasoning, and exact-shape semantic contradictions. Parallel specialist prompt requirements and both source artifact identities/hashes/sizes are asserted exactly. The earlier CRA residual paragraph is superseded: this source's behavioral assertions are now represented, pending passing native verification. No source artifacts changed and no independent judge/live execution was performed.

## Eval clients (partial)

`ai-platform-gateway --test eval_migration_parity_clients`: seven tests use isolated loopback responders and actual native clients. Retains RAGAS payload and infrastructure result parsing, safe selector forwarding, secret override rejection before transport, HTTP 500 handling, all five malformed result collections, and injected internal-secret wire binding. Preserves LLM judge tool-free bounded request, tenant-bound JWT, disabled client no-dispatch, and delegated claim subject/actor/tenant/role/expiry requirements.

Remaining source obligations: `test_kb_ragas_client.py` environment-to-signer configuration wiring; `test_eval_llm_client.py` invalid timeout config fallback and outbox factory wiring; `test_eval_candidate_identity.py` canonical introspection and no-write matrix plus explicit configured token precedence. Source Python factory/mocking mechanics need not be reproduced, but their configuration/security behavior still requires targeted coverage. Root must execute the loopback tests; no network calls were performed while authoring. Authfoundation confirmed these three source matrices were not owned in its lane.

Client follow-on: four more tests (total 11) retain candidate identity security matrices: both JWT/API-key tenant mismatch directions, all nine malformed/unscoped identities, 302/401/403/500 safe failures with no redirect/write, and repeat introspection on a reused client. Real native `EvalCandidateClient::run` is exercised with an inert lazy repository handle, and all cases must stop at the one captured GET before repository access. Source service-identity actor resolution, transport timeout/non-JSON, and outbox runner no-ingest composition remain outstanding; no whole identity-source retirement claim yet.

Client target now totals 12: raw isolated loopback responses additionally prove safe non-JSON and transport-disconnect errors. This covers the transport error redaction class without running the source mock's 300-second native timeout. Explicit timeout expiration is still not asserted by this target.

## Subsequent parent verification

Read actual `eval-parity79.log`: golden CLI4 passed after provenance sorting repair; judges3 (including its child matrix), RAG8, retrieval11, stateful17 and golden23 passed. This verifies earlier batches only. Batch4 targets remain queued and are not covered by log79.

RAGAS wrapper audit: native internal client returns `{judge_model, results}`; historical Python helper returned a list of typed result objects. The only native production consumers are `eval_scoring.rs` (reads `result["results"]`) and `eval_knowledge.rs` (projects `{judge_model, results:[{metric,score,explanation,label}]}`). Historical `kb_ragas_service.py::score_retrieval_with_kb_ragas` constructs the same external HTTP projection from the list. The test therefore reads the native internal `results` field without changing the public wire expectation or dropping result assertions.

Actual `eval90.log`: CRA compiled, 11 passed and two failed solely on historical diagnostic wording. Reviewed production guards: official() checks HTTPS plus exact allowed host before artifact fetch; validate_fixture compares frozen fixture-snapshot and artifact hashes before scoring. Tests now require exact native diagnostics `not allowlisted official HTTPS URL` and `provenance hashes mismatch`, respectively. In the latter mutation the artifact hash stays unchanged, so the independent snapshot mismatch remains specifically exercised. No guard, mutation, threshold, or error acceptance was relaxed. Rerun pending; remaining eval targets did not execute after CRA failure.

## Frozen verification update: eval95

Parent's actual serialized `eval95.log` passed CRA13, finance7, producer13, and existing runtime_contract_source7. This verifies the new complete CRA/finance/producer source matrices and the repaired CRA evaluator guard. It does not verify the queued gateway targets or turn the recovered legacy test inventory into migrated tests. Root preserves the unported source inventory separately; no blanket retirement claim applies.
