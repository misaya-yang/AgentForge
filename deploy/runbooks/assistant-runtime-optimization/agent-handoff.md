# Assistant Runtime Optimization Agent Handoff

## Planner Notes

The repo already has a stronger runtime than the pasted Claude summary assumed. Do not rebuild foundations that now exist: runtime memory, MCP/skill gates, assistant/langgraph/rag trace families, Eval datasets/evaluators, KB RAGAS, and Eval UI are present. The plan should close loops around middleware lifecycle, approval/resume, trace feedback, checkpoint/resume, and performance/cost telemetry.

## Current Status

`ARO-00` passed with actor and critic evidence. The baseline gate exposed and fixed a small contract drift:

- `apps/assistant-service/src/assistant_service/api/routes/chat.py` now forwards request `traceparent` into `AssistantConfig` and derives `otel_trace_id`.
- `web/src/pages/assistant/sse-events.ts` now includes the RAG retrieval stream event names emitted by the backend.
- `tests/services/assistant/test_agent_loop_golden.py` and `tests/services/assistant/test_assistant_service.py` protect those contracts.

`ARO-01` passed with actor and critic evidence. The runtime now has:

- `MiddlewareChain.run_on_stream_event()` and `run_on_error()` wired into AgentLoop streaming-first event/error paths.
- Non-default reliability middlewares in `apps/assistant-service/src/assistant_service/core/agent/middlewares/harness.py`.
- Middleware `CONFIRM` approval persistence with `approval_id`, approved resume, argument matching, and single-use approval consumption.

`ARO-02` passed with actor and critic evidence. Eval feedback now has:

- `src/services/eval/trace_feedback.py` for bounded failure classification, clustering, redacted dataset-case construction, proposed harness/profile changes, and candidate gate evaluation.
- `/api/v1/eval/trace-feedback:preview`, which is tenant/user scoped and eval-run protected.
- Redacted `EvalExampleImportItem` preview payloads that carry source trace IDs, replay/evaluator metadata, tenant metadata, and proposed-only review state.
- No schema migration, SaaS dependency, production trace access, deployment, or automatic profile application.

`ARO-03` passed with actor and critic evidence. Durable runtime now has:

- `database/migrations/067_assistant_run_checkpoints.sql` for additive run checkpoints.
- `AssistantExecutionGateway.save_run_checkpoint()`, `get_run_checkpoint()`, and `prepare_run_resume()`.
- AgentLoop checkpoint writes at run/model/tool/approval/terminal boundaries.
- `/api/v1/assistant/runs/{run_id}/resume`, which returns a non-executing resume plan and records `resume_blocked` when resume cannot proceed.
- Sanitized checkpoint payloads that store summaries/hashes rather than raw prompts, full messages, or full tool arguments.

`ARO-04` passed with actor and critic evidence. Performance/cost/context evidence now has:

- Stable prompt-prefix and tool-schema identity hashes emitted on `context_budget`.
- Estimated context input tokens, model context window, and context utilization emitted on `context_budget`.
- Provider cache token metrics normalized across OpenAI/DashScope, Gemini, and Anthropic usage payloads.
- Deterministic tool selector tie-break ordering for equal score/tier tools.
- No enabled adaptive model routing, embedding selector rollout, live provider test, or unsupported cost-savings claim.

`ARO-05` passed with actor and critic evidence. Release evidence now has:

- `make verify-eval-dev` passing across eval API/trace, evaluator, golden regression, assistant trace, web lint, and web type-check gates.
- `make validate-example-config` passing for the public example configuration.
- `deploy/runbooks/assistant-runtime-optimization/operating-model.md` documenting runtime decisions, rollback boundaries, trusted commands, SLO/no-go thresholds, no-deploy rules, and release backlog.
- ARO-05 phase completion gate and full harness completion gate passing.
- No deployment, production migration, provider load test, or production data mutation.

## Generator Target

No generator phase remains. Future work should start a new, explicitly scoped goal for deployment, dashboarding, or adaptive-routing experiments rather than editing this completed harness in place.

If a future agent resumes this folder, it should inspect the reports and completion gates first, not re-run implementation phases unless the user requests a repair.

## Critic Target

No critic phase remains. A future critic should review the full completion gate and all six actor/critic report pairs before any deployment handoff.

## Next Unlock

No dependent phase remains. The full assistant runtime optimization goal is complete after the full harness completion gate passes.
