# Assistant Runtime Optimization Continuity Ledger

## Phase Chain

| Phase | Feature | Depends On | Unlocks | Handoff Boundary |
| --- | --- | --- | --- | --- |
| ARO-00 | ARO-F001 | none | ARO-01 | Freeze current runtime facts, research synthesis, validation commands, and stale-summary corrections. |
| ARO-01 | ARO-F002 | ARO-00 | ARO-02 | Implement middleware lifecycle hooks and approval/resume without broad AgentLoop rewrite. |
| ARO-02 | ARO-F003 | ARO-01 | ARO-03 | Build trace-to-dataset/eval/harness feedback on top of existing trace families. |
| ARO-03 | ARO-F004 | ARO-02 | ARO-04 | Add checkpoint/resume and idempotency around long runs and approval pauses. |
| ARO-04 | ARO-F005 | ARO-03 | ARO-05 | Make context/cache/reasoning/tool-routing optimization measurable and eval-gated. |
| ARO-05 | ARO-F006 | ARO-04 | none | Run whole-demand regression and publish operating docs/SLO no-go thresholds. |

## Current Code Boundary Decisions

- `deploy/runbooks/assistant-runtime-optimization` is the durable harness path because root `docs/*` and `docs_project/` are ignored by `.gitignore`.
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py` remains the canonical runtime loop; early phases should avoid a sweeping rewrite and instead introduce stable hook boundaries.
- `apps/assistant-service/src/assistant_service/core/agent/middleware.py` is the intended expansion point for harness lifecycle behavior.
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py` is the source of truth for run/command/approval persistence.
- `src/api/v1/eval.py`, `src/services/eval/`, and `packages/ai-gateway-core/src/ai_gateway_core/eval/` are the natural trace/eval feedback surfaces.
- `src/services/eval/langgraph_trace_capture.py` and `src/services/eval/rag_trace_capture.py` already exist; future work should extend and validate them instead of treating them as absent.
- `apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py` and `core/rag/context_engine.py` are the starting points for cache/context metrics.
- ARO-00 restored the assistant stream/config contract baseline before implementation phases: backend RAG retrieval events are represented in frontend assistant event constants and golden tests, and chat route traceparent now reaches `AssistantConfig`.
- ARO-01 inherits a passing baseline for assistant event vocabulary, trace capture, streaming-first contract, eval trace family tests, targeted Python lint, and frontend type-check.
- ARO-01 completed the middleware lifecycle boundary. Future phases can use `on_stream_event` and `on_error` instead of adding more direct AgentLoop event side effects.
- ARO-01 approval resume is now single-use and argument-matched. ARO-03 checkpoint/resume must preserve this invariant and must not replay consumed approvals.
- ARO-02 completed the trace feedback preview boundary in `src/services/eval/trace_feedback.py` and `/api/v1/eval/trace-feedback:preview`; downstream phases should reuse its bounded failure modes instead of inventing new names for approval pauses, tool errors, loop detection, context overflow, RAG miss, empty output, low score, or latency regression.
- ARO-02 dataset-case output is import-ready but non-mutating. Future phases must keep profile/harness changes proposed until review/eval/rollback evidence exists.
- ARO-03 added additive `assistant_run_checkpoints` storage and a gateway/API resume-preparation contract. Checkpoints must remain bounded summaries/hashes and must not grow into raw prompt/message/tool-argument storage.
- ARO-03 preserves ARO-01's single-use approval consumption by keeping `/runs/{run_id}/resume` non-executing; actual side effects still pass through the execution gateway and command de-dupe.
- ARO-04 added prompt-prefix/tool-schema identity hashes, context utilization, and provider cache token normalization to existing assistant events/usage. Downstream release evidence can rely on `context_budget.prompt_prefix_hash`, `context_budget.context_utilization`, and `usage.cached_input_tokens` / provider-specific cache read/write fields.
- ARO-04 did not enable adaptive model routing or embedding-based tool selection. Those remain future controlled rollouts requiring eval proof and rollback config.
- The original broad ARO-04 ruff command still fails on unrelated pre-existing lint in untouched tool/test files; use the focused ARO-04 ruff command from the phase contract/report for this phase's evidence.
- ARO-05 published `operating-model.md` as the release operating surface. It records trusted verification commands, rollback boundaries, SLO/no-go thresholds, no-deploy rules, and release backlog.
- ARO-05 regression evidence uses `make verify-eval-dev` and `make validate-example-config`; web lint currently passes with warnings only, not errors.
- No production deployment, production migration, provider load test, or external service mutation was performed by this harness.
- Post-completion review tightened the ARO-03 resume contract: checkpoint approval readiness must compare the approved arguments hash with the checkpoint `pending_tool.arguments_hash`; a matching approval ID alone is not enough.
- Post-completion review tightened ARO-01 stream-event middleware semantics: if `on_stream_event` rewrites a terminal success into `run_error`, `AgentLoop` must persist the run as failed and must not emit a contradictory `run_finished`.
- Post-completion review tightened ARO-02 trace feedback redaction: metadata keys containing authorization, token, secret, password, cookie, credential, api_key, or apikey must be omitted from generated dataset cases and preview responses.
- Post-completion review clarified the ARO-03 resume API: `POST /runs/{run_id}/resume` is a non-executing probe and must allow an empty request body when no approval ID is available yet.

## Writeback Rules

Every phase runner must update this ledger when:

- a file/path boundary changes;
- a validation command changes;
- a schema or API contract is added;
- a downstream phase inherits new assumptions;
- a blocker prevents dependency unlock.
