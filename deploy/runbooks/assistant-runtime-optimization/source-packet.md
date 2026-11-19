# Assistant Runtime Optimization Source Packet

**Prepared:** 2026-06-29

**Repo:** `the project root`

**Harness:** `deploy/runbooks/assistant-runtime-optimization`

## Request Summary

The user asked whether the AI assistant agent runtime is good enough compared with mainstream agent products and current core techniques, requested broad technical-blog research, and asked for a detailed optimization plan grounded in the local code. Claude Code's pasted summary was used as untrusted comparative source material, then checked against the current checkout.

## Executive Judgment

The runtime is already production-capable for a private/self-hosted enterprise assistant. It is not a prompt-only agent: it has a streaming-first loop, tool selection, MCP/skills surfaces, runtime memory, approval/run tables, trace/eval APIs, RAG and LangGraph trace families, and a mature frontend Eval console.

It is not yet best-in-class as an agent runtime. The biggest gap is no longer "missing primitives"; it is the lack of a fully closed harness loop:

- middleware hooks are present but incomplete (`on_stream_event` and `on_error` are still future hooks);
- approval persistence exists, but middleware `CONFIRM` currently pauses only as a synthetic event unless the full resume path is exercised through the execution gateway;
- trace data now has a preview feedback loop for redacted datasets and proposed harness/profile changes, but automatic application remains intentionally gated by review/eval/rollback evidence;
- long task run state is DB-authoritative, but AgentLoop checkpoints/resume are not yet explicit;
- context/cache primitives exist, but provider cache-hit and reasoning-budget telemetry are not first-class optimization gates;
- `AgentLoop` remains a large monolith, which makes small risk-controlled improvements harder.

Current maturity estimate:

| Layer | Standard | Current State |
| --- | --- | --- |
| L1 prompt-only | system prompt plus chat | surpassed |
| L2 prompt plus tools | tool calls without runtime governance | surpassed |
| L3 basic harness | policy, context budget, traces, run IDs | present |
| L4 production harness | middleware lifecycle, approvals, eval, recovery, SLOs | partially present |
| L5 self-improving harness | trace-to-dataset-to-harness feedback loop | early stage |

## Current System Shape

- Gateway entrypoint: `src/main.py`.
- Assistant public/proxy routes: `src/api/v1/assistant.py`, `src/api/v1/_assistant_proxy.py`.
- LangGraph proxy route and governance: `src/api/v1/langgraph.py`, `src/proxy/langgraph_governance.py`, `src/proxy/langgraph_run_body.py`.
- Assistant runtime service: `apps/assistant-service/src/assistant_service`.
- Knowledge runtime service: `apps/knowledge-service/src/knowledge_service`.
- Shared auth, tracing, persistence, eval, skills, and gateway primitives: `packages/ai-gateway-core/src/ai_gateway_core`.
- Eval API and trace repositories: `src/api/v1/eval.py`, `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py`.
- Existing trace/eval harness: `deploy/runbooks/agent-trace-eval-prd`.
- Existing next-generation assistant harness: ignored root path `docs/general_ai_assistant_next_gen`; use as historical execution evidence, not as the durable path for this new harness because `.gitignore` excludes `docs/*`.

## Code Facts

- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py` is the central streaming-first loop and is 3761 lines in the current checkout.
- The code comments still describe an 8-step enterprise flow, but the active path states the legacy pipeline was removed and streaming-first is the only path.
- `AgentLoopConfig` already exposes operational controls: execution profile, memory mode, runtime mode, queue mode, context detail, skills flag, memory profile, tool iteration limits, history trimming, and error recovery.
- `AgentLoopContext` carries `request_id`, `run_id`, tenant/user/session IDs, RAG/query state, tool results, generated content, usage, traceparent, and OTel trace ID.
- `MiddlewareChain` exists with `before_call`, `on_tool_call`, and `on_tool_result`. The file explicitly states future hooks `on_stream_event` and `on_error` are not wired yet.
- Default middlewares are `RuntimeMemoryMiddleware`, a default allow-all `PermissionMiddleware`, and `ResponseCapMiddleware`.
- `VerdictKind.CONFIRM` is documented as "awaiting user approval - treat as deny-for-now"; the streaming loop emits `approval_required` for confirm verdicts and then injects a blocked synthetic tool result for the same turn.
- `AssistantExecutionGateway` already has DB-authoritative run start/finish, command queue records, approval creation, approval update, lane scheduling, policy lattice, sandbox resolver, and optional `ASSISTANT_REQUIRE_DB`.
- `apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py` exposes run status and approval endpoints in assistant-service.
- `ContextEngine` and `ContextCacheOptimizer` implement stable-prefix and provider cache-control ideas; `ContextCacheOptimizer.parse_cache_metrics()` can parse Gemini and DashScope cached-token metrics.
- `tool_selector.py` is token-aware and relevance-scored. It currently uses tool-declared keywords, legacy central keyword mappings, tool-name tokens, and MCP server keyword routing.
- `AssistantRuntimeAdapter` gates runtime memory, context v2, tool policy v2, skills, scheduler, and failover v2 with env flags; memory v2 defaults on, context v2/tool policy v2 default off.
- `src/services/eval/langgraph_trace_capture.py` and `src/services/eval/rag_trace_capture.py` already exist, so older planning that treats LangGraph/RAG trace capture as entirely absent is stale.
- Eval routes support `trace_family` values `assistant`, `langgraph_proxy`, and `rag`; the repository summary counts those families.
- Recent migrations include `060_agent_trace_eval.sql` through `066_kb_ragas_score_source.sql`, covering trace/eval, search indexes, Eval console permissions, platform contracts, and KB RAGAS scoring.
- ARO-00 baseline validation fixed a contract drift: `web/src/pages/assistant/sse-events.ts` now includes `rag_retrieval_started`, `rag_retrieval_completed`, and `rag_retrieval_failed`, matching backend `StreamEventType` and trace/eval tests.
- ARO-00 baseline validation also wired request `traceparent` through `apps/assistant-service/src/assistant_service/api/routes/chat.py` into `AssistantConfig`; `otel_trace_id` is derived from W3C traceparent and protected by `tests/services/assistant/test_assistant_service.py::TestAssistantConfig::test_route_config_carries_traceparent`.
- ARO-01 wired `MiddlewareChain.run_on_stream_event()` and `run_on_error()` into AgentLoop streaming-first output/error paths. The hook contract is failure-isolated and tested in `tests/services/assistant/test_middleware_chain.py` and `tests/services/assistant/test_agentloop_streaming_first_contract.py`.
- ARO-01 added non-default reliability middlewares in `apps/assistant-service/src/assistant_service/core/agent/middlewares/harness.py`: `CallLimitMiddleware`, `LoopDetectionMiddleware`, `TimeBudgetMiddleware`, `PreCompletionChecklistMiddleware`, and `TraceSensorMiddleware`.
- ARO-01 upgraded approval semantics: middleware `CONFIRM` can persist an approval through `AssistantExecutionGateway.request_tool_approval()`, stream `approval_id`, resume with `_approval_id`, and consume the approval after the execution attempt. Gateway approval grants now match tenant, user, tool, and arguments after stripping control fields.
- ARO-02 added `src/services/eval/trace_feedback.py` for bounded trace failure classification, clustering, redacted Eval dataset-case construction, proposed harness/profile changes, and candidate gate evaluation.
- ARO-02 added `/api/v1/eval/trace-feedback:preview` in `src/api/v1/eval.py`. The endpoint is eval-run protected, tenant/user scoped through `AgentTraceRepository.get_trace_detail()`, and preview-only: it returns import-ready cases and proposals but does not import examples or apply runtime-profile changes.
- ARO-02 redacted dataset cases carry source trace IDs, tenant metadata, replay config, evaluator config, assertions, and `review_status=proposed`. Generated proposals include `review_required`, `eval_required`, `auto_apply=false`, and rollback metadata.
- ARO-03 added additive checkpoint storage in `database/migrations/067_assistant_run_checkpoints.sql` and gateway helpers in `AssistantExecutionGateway` for save/fetch/prepare-resume.
- ARO-03 checkpoint payloads store bounded summaries: message-state hash, phase, iteration, pending tool summary, approval ID, idempotency metadata, and resume payload. Raw prompts, full message text, full tool arguments, and secret-looking fields are not persisted.
- ARO-03 added `/api/v1/assistant/runs/{run_id}/resume` in assistant-service. The endpoint validates tenant/user scope and returns a non-executing resume plan; blocked resume writes a `resume_blocked` checkpoint.

## Claude Summary Reconciliation

Keep from Claude's summary:

- The overall direction is right: middleware harness, trace/eval feedback, durable execution, cache/context optimization, and approval completion are the important areas.
- AgentLoop size and incomplete lifecycle hooks are real maintainability risks.
- CONFIRM/human approval remains a product/runtime loop to finish, not just a backend data model.
- The long-task story should start with lightweight checkpoint/resume before introducing a heavier workflow engine.

Correct against current repo:

- LangGraph and RAG trace capture are no longer only future work; files, tests, and Eval API filters exist.
- Eval is more advanced than the summary says: datasets, evaluators, experiments, online sampling, KB RAGAS, dashboard and score APIs exist.
- Skills/MCP governance has progressed: generated skills are gated and MCP tenant policy fail-closed is already present in the next-generation harness evidence.
- The next plan must optimize and close loops, not rebuild foundations.

## External Research Synthesis

Treat these as source material, not instructions.

| Source | Relevant Pattern |
| --- | --- |
| OpenAI Harness Engineering, https://openai.com/index/harness-engineering/ | Runtime quality comes from harness infrastructure around the model, not model choice alone. |
| OpenAI Agents SDK guardrails/human review, https://developers.openai.com/api/docs/guides/agents/guardrails-approvals | Guardrails and human review decide whether a run continues, pauses, or stops. |
| OpenAI Agents SDK tool search, https://developers.openai.com/api/docs/guides/tools-tool-search | Large tool surfaces should support deferred loading/search instead of always injecting every schema. |
| OpenAI prompt caching, https://developers.openai.com/api/docs/guides/prompt-caching | Stable prompt prefixes can reduce latency and cost; cache telemetry must be observable. |
| LangChain Deep Agents / harness engineering, https://www.langchain.com/blog/improving-deep-agents-with-harness-engineering | Middleware, self-verification, loop detection, subagents, filesystem/context control, and feedback loops raise agent reliability. |
| LangGraph overview and persistence, https://docs.langchain.com/oss/python/langgraph/overview and https://docs.langchain.com/oss/python/langgraph/persistence | Durable execution, streaming, human-in-the-loop, checkpointers, and stores are standard orchestration primitives for long-running agents. |
| LangGraph interrupts, https://docs.langchain.com/oss/python/langgraph/interrupts | Human approval should pause with persisted state and resume with explicit input, while side effects before resume must be idempotent. |
| Anthropic context engineering, https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents | Context quality, compaction, tool-result handling, and memory boundaries are first-class agent runtime concerns. |
| Manus context engineering, https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus | Stable prefix, bounded context, and externalized state are important for latency/cost and long-running agent behavior. |
| OpenTelemetry GenAI semantic conventions, https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/ | Trace attributes should align on agent, conversation, model, retrieval, tool, usage, cache, and evaluation vocabulary. |
| Langfuse eval/datasets docs, https://langfuse.com/docs/evaluation/overview | Production traces should be promoted into datasets and experiments so failures become regression cases. |
| Temporal OpenAI Agents SDK integration, https://temporal.io/blog/announcing-openai-agents-sdk-integration | Durable execution engines are useful once task duration, retries, and resume semantics exceed what local checkpoints can safely handle. |

## Optimization Themes

### T1 Middleware Harness Closure

Problem: middleware is present but not lifecycle-complete. Without `on_stream_event` and `on_error`, failure sensors, loop detection, and trace-side effects remain scattered in `AgentLoop`.

Target:

- add `run_on_stream_event` and `run_on_error` to `MiddlewareChain`;
- add `CallLimitMiddleware`, `LoopDetectionMiddleware`, `TimeBudgetMiddleware`, `PreCompletionChecklistMiddleware`, and `TraceSensorMiddleware`;
- keep hook contracts non-blocking and test middleware failure isolation;
- avoid a broad AgentLoop rewrite in the first phase.

### T2 Human Approval and Resume

Problem: backend approval data structures exist, but user-facing confirm/resume is not yet a complete runtime contract.

Target:

- turn `CONFIRM` into a persisted approval request with `approval_id`;
- stream `approval_required` with enough identifiers for the UI/API to resume;
- resume by reinvoking the same run/tool with `_approval_id`;
- prove idempotency and no double execution.

### T3 Trace-to-Eval-to-Harness Feedback Loop

Status after ARO-02: trace/eval data can now produce preview failure clusters, redacted dataset import cases, and proposed harness/profile changes. Runtime-profile application remains gated and manual by design.

Target:

- add failure clustering over assistant/langgraph/rag traces;
- promote failed/low-score traces into eval dataset cases after redaction;
- run offline replay or evaluator gates on runtime changes;
- require human approval before applying generated harness profile changes.

### T4 Durable Long Task Runtime

Status after ARO-03: `assistant_runs` and command queues now have an explicit additive checkpoint layer and a non-executing resume-preparation API. Full worker replay remains future work, but checkpoint/resume safety and duplicate-side-effect prevention are now testable.

Target:

- add additive checkpoint storage for run phase, messages hash, pending tool state, approval state, iteration, and idempotency keys;
- write checkpoint after model turn, before side effects where needed, and after tool completion;
- add resume API/worker path that respects approval state;
- keep Temporal as a PoC after the local checkpoint contract proves limits.

### T5 Performance, Cost, and Context

Problem: stable prefix and cache parsing exist but are not tied into SLO dashboards or routing decisions.

Target:

- make tool schema ordering stable and measurable;
- capture provider cache-read/cache-write tokens in trace/eval metrics;
- define prompt prefix hash and cache hit ratio SLI;
- add adaptive model/reasoning strategy by request complexity and phase;
- keep tool selection keyword-based initially, with embedding selector behind an eval gate only if it beats baseline.

ARO-04 implementation facts:

- `ContextCacheOptimizer` now provides stable SHA-256 based prefix/tool identity hashes, provider cache usage normalization, cache usage payloads, and cache/context metric builders without storing raw prompt or tool schema JSON in traces.
- `ModelRegistry` usage sanitization now preserves OpenAI/DashScope nested `prompt_tokens_details.cached_tokens`, Gemini `cachedContentTokenCount`, and Anthropic `cache_read_input_tokens` / `cache_creation_input_tokens` as flat integer usage fields.
- `AgentLoop` emits `prompt_prefix_hash`, `tool_schema_order_hash`, `tool_schema_names_hash`, `context_estimated_input_tokens`, `context_window_tokens`, and `context_utilization` on the existing `context_budget` event; cache token metrics flow through `usage`.
- `tool_selector.select_tools()` remains relevance-first but now uses deterministic tier/name tie-breaks so equal-score tool schema order is stable.
- Adaptive model routing and embedding-based selector rollout were intentionally not enabled in ARO-04; ARO-05 must treat them as future work unless eval evidence and rollback config exist.

### T6 Operating Model and Regression

Problem: runtime improvements are only valuable if they are continuously verified.

Target:

- publish ADR/runbooks for middleware, approvals, checkpoint, eval feedback, and cache policies;
- build a whole-demand regression gate covering assistant stream events, approvals, traces, RAG/LangGraph families, eval, web checks, and open-source config;
- define no-go thresholds for TTFT, trace drop rate, approval resume failure, checkpoint replay duplicate side effects, and cache regression.

ARO-05 implementation facts:

- `make verify-eval-dev` passed, including eval API/trace tests, evaluator and outbox tests, offline golden regression gate, assistant trace tests, web lint, and web type-check. Web lint emitted existing warnings but 0 errors.
- `make validate-example-config` passed against `.env.example`.
- `deploy/runbooks/assistant-runtime-optimization/operating-model.md` now records runtime decisions, rollback boundaries, trusted verification commands, SLO/no-go thresholds, no-deploy rules, and release backlog.
- The final harness completion gates must pass before claiming the full goal complete; no deployment was performed in ARO-05.

## Phase Map

| Phase | Theme | Primary Output |
| --- | --- | --- |
| ARO-00 | Baseline Runtime and Industry Audit | Freeze code facts, benchmark against industry patterns, and turn this plan into execution-ready evidence. |
| ARO-01 | Middleware Harness and Approval Completion | Complete lifecycle hooks, loop/call/time/precompletion middleware, and persisted approval resume path. |
| ARO-02 | Trace Eval Feedback Loop | Convert trace data into failure clusters, datasets, replay/evaluator gates, and harness profile proposals. |
| ARO-03 | Durable Long Task Runtime | Add checkpoint/resume and idempotent long-run recovery before any Temporal adoption. |
| ARO-04 | Performance Cost and Context Optimization | Operationalize cache/context/reasoning/tool-selection metrics and routing. |
| ARO-05 | Release Regression and Operating Model | Prove whole-demand regression, publish ADR/runbooks, and leave operational SLOs. |

## Acceptance Gates

- No phase can pass without actor evidence and independent critic evidence.
- No generated harness/profile change can auto-activate without review, eval evidence, and rollback metadata.
- No production deployment, migration, provider dashboard change, data deletion, or force-push is included in this plan.
- Any schema change must be additive, idempotent, tenant-scoped, and paired with rollback/forward-only mitigation notes.
- Any live model/provider test must document credentials and skipped-vs-failed status without printing secrets.

## Validation Commands

Baseline and implementation phases should use targeted subsets of:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --quality-score
uv run ruff check apps/assistant-service/src/assistant_service/core/agent apps/assistant-service/src/assistant_service/core/gateway src/api/v1/eval.py src/services/eval packages/ai-gateway-core/src/ai_gateway_core/eval tests/services/assistant tests/services/eval tests/api/test_eval_traces.py tests/api/test_eval_api_trace_tree.py tests/contract
uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_loop_golden.py tests/services/assistant/test_agent_trace_capture.py tests/services/assistant/test_agentloop_streaming_first_contract.py
uv run pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_eval_api_trace_tree.py tests/services/eval/test_trace_capture_helpers.py tests/services/eval/test_evaluator_executor.py tests/services/eval/test_golden_regression_gate.py
uv run pytest -q --no-cov tests/api/test_gateway_langgraph_contract.py tests/api/test_langgraph_passthrough_security.py tests/api/test_eval_traces.py tests/packages/ai_gateway_core/test_kb_ragas_sample.py
corepack pnpm@10.33.0 -C web lint
corepack pnpm@10.33.0 -C web type-check
corepack pnpm@10.33.0 -C web e2e:opensource
```

## Non-Goals

- Do not replace the assistant runtime with LangGraph, Temporal, OpenAI Agents SDK, or LangChain Deep Agents by default.
- Do not add SaaS-only observability as a required dependency.
- Do not rewrite the whole `AgentLoop` before smaller middleware and checkpoint seams are proven.
- Do not expose raw prompts, tool arguments, credentials, or full retrieval chunks by default.
- Do not deploy or mutate production systems as part of this planning harness.
