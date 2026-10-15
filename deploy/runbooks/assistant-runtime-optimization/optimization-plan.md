# AI Assistant Agent Runtime Optimization Plan

**Date:** 2026-06-29

**Repo:** `the project root`

## Bottom Line

当前 AI assistant agent runtime 已经不是普通 demo，生产私有化可用性比较强：有 streaming-first AgentLoop、工具治理、skills/MCP、runtime memory、DB-authoritative runs、trace/eval、RAG/LangGraph trace family、Eval console。和主流产品相比，差距不在「有没有 agent loop」，而在「harness 是否闭环」。

最值得优化的方向是：

1. 完整 middleware 生命周期：把错误、流事件、调用上限、循环检测、完成前自检从 AgentLoop 单体里抽成可测 middleware。
2. 人机审批闭环：把 `CONFIRM` 从「当前 turn 合成 blocked tool result」升级成可持久化、可恢复、不会重复执行副作用的 approval/resume。
3. Trace -> Eval -> Harness 外环：失败 traces 要能自动聚类、脱敏入 dataset、回放验证，然后产出待审批的 harness/profile 改进。
4. 长任务 durable runtime：先做轻量 checkpoint/resume，再决定是否接 Temporal。
5. 性能成本优化：把 stable prefix/cache/reasoning/tool-selection 变成可观测 SLI，而不是代码里存在但运营不可见。

## Industry Comparison

| Dimension | Current Runtime | Strong Industry Pattern | Gap |
| --- | --- | --- | --- |
| Agent loop | Custom streaming-first `AgentLoop` | LangChain/Deep Agents middleware and graph runtimes | loop still too monolithic; lifecycle hooks incomplete |
| Tool surface | Token-aware selector, MCP, skills | OpenAI deferred tool search; MCP tool search; policy guards | selector is still mostly keyword/scoring; no eval-gated embedding selector |
| Human approval | DB approvals and API exist | OpenAI human review, LangGraph interrupts/checkpoints | persisted resume and no-double-execute proof need completion |
| Context engineering | stable prefix, context budget, cache optimizer | Manus/Anthropic context engineering, prompt caching | provider cache metrics not a release SLI |
| Observability | first-party Eval and trace families | LangSmith/Langfuse trace-to-dataset/eval loops | feedback loop does not yet auto-generate versioned harness proposals |
| Durable execution | run DB, command queue, task worker | LangGraph checkpointers, Temporal workflows | AgentLoop checkpoint/resume is not explicit |
| Safety | tenant policy, sandbox, redaction, generated skill gates | guardrails per input/output/tool and HITL gates | middleware error/stream hooks and approval flow need hardening |

## Current Strengths to Preserve

- Streaming-first contract and AG-UI-like events are a strong UX foundation.
- Eval/trace is self-hosted and already more advanced than a plain logging solution.
- `trace_family` now covers `assistant`, `langgraph_proxy`, and `rag`; do not regress that to assistant-only.
- Runtime memory, generated-skill lifecycle gates, MCP default-deny, and source redaction are the right enterprise posture.
- Keeping gateway + assistant-service + knowledge-service boundaries is preferable to a framework replacement.

## Main Risks

1. `AgentLoop` size creates hidden coupling. A 3761-line loop makes every reliability change riskier.
2. Middleware contract is not yet the source of truth for all harness behavior.
3. Approval pause/resume semantics can double-execute tools unless checkpoint and idempotency are explicit.
4. Trace data may become a dashboard-only asset unless it feeds datasets and regression gates.
5. Cache/reasoning optimizations can lower quality unless guarded by eval cases and per-model telemetry.

## Roadmap

### Phase ARO-00: Baseline Runtime and Industry Audit

Purpose: freeze current facts before implementation.

Outputs:

- baseline report with exact code paths, validation commands, and stale parts of Claude's summary;
- updated source packet and continuity ledger;
- strict harness validation.

Exit gate:

- `validate_harness_prd.py ... --strict --quality-score` passes.

### Phase ARO-01: Middleware Harness and Approval Completion

Purpose: close the core harness lifecycle.

Implementation scope:

- `apps/assistant-service/src/assistant_service/core/agent/middleware.py`
- `apps/assistant-service/src/assistant_service/core/agent/middlewares/`
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`
- `apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py`

Required work:

- add `run_on_stream_event` and `run_on_error`;
- add call limit, loop detection, time budget, pre-completion checklist, and trace sensor middleware;
- make middleware exceptions observable but non-fatal where appropriate;
- persist `CONFIRM` as approval with `approval_id`;
- prove resume with `_approval_id` executes once and only once.

Acceptance:

- focused assistant tests pass;
- stream event schema does not regress;
- approval-required -> approve -> resume has actor and critic evidence.

### Phase ARO-02: Trace Eval Feedback Loop

Purpose: make trace/eval drive improvement.

Implementation scope:

- `src/api/v1/eval.py`
- `src/services/eval/`
- `packages/ai-gateway-core/src/ai_gateway_core/eval/`
- `packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_trace_repository.py`
- `web/src/api/eval.ts`
- `web/src/pages/eval/`

Required work:

- failure-mode clustering for assistant/langgraph/rag traces;
- trace-to-dataset promotion with redaction and tenant scope;
- replay/evaluator gate for harness changes;
- UI panel for failure patterns and candidate harness-profile changes;
- no auto-activation without review/eval/rollback metadata.

Acceptance:

- failed trace becomes dataset case;
- dataset/evaluator run blocks a bad harness profile;
- UI shows bounded, redacted failure patterns.

### Phase ARO-03: Durable Long Task Runtime

Purpose: let long runs survive process restart, approval pauses, and tool failures.

Implementation scope:

- `database/migrations/`
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`
- `apps/assistant-service/src/assistant_service/api/routes/runs_approvals.py`
- assistant API/proxy routes as needed.

Required work:

- additive checkpoint table or JSONB checkpoint column;
- checkpoint after model turns, before/after side-effecting tools, and at approval pause;
- resume endpoint/worker path;
- idempotency keys for tool calls;
- crash/restart simulation in tests.

Acceptance:

- interrupted run resumes from checkpoint;
- approval resume does not duplicate a tool action;
- failed resume returns a user-visible blocked state and leaves trace evidence.

### Phase ARO-04: Performance Cost and Context Optimization

Purpose: turn existing cache/context ideas into measurable runtime policy.

Implementation scope:

- `apps/assistant-service/src/assistant_service/core/rag/context_engine.py`
- `apps/assistant-service/src/assistant_service/core/quality/cache_optimizer.py`
- `apps/assistant-service/src/assistant_service/core/models/model_registry.py`
- `apps/assistant-service/src/assistant_service/core/tools/tool_selector.py`
- trace/eval metrics surfaces.

Required work:

- stable tool-schema ordering and prompt-prefix hash;
- provider cache read/write token extraction into traces;
- cache hit ratio, TTFT, context utilization, and reasoning token SLI;
- adaptive model/reasoning policy behind config;
- optional embedding tool selector only after offline eval beats keyword baseline.

Acceptance:

- cache metrics appear in trace/eval;
- no quality drop on golden eval cases;
- routing policy can be disabled by config.

### Phase ARO-05: Release Regression and Operating Model

Purpose: prove the whole runtime and leave maintainable operating docs.

Implementation scope:

- `deploy/runbooks/`
- targeted tests and CI commands;
- web Eval/assistant checks;
- ADR/runbook docs.

Required work:

- publish ADRs for middleware, approvals, checkpoint/resume, trace feedback, cache/reasoning policy;
- whole-demand regression across ARO-F001 to ARO-F006;
- SLO/no-go thresholds for TTFT, trace drop, approval resume, checkpoint duplicate execution, cache regression, and eval failure rate;
- fresh-window handoff prompt and rollback notes.

Acceptance:

- backend focused tests pass;
- web lint/type-check/e2e pass when UI changed;
- strict harness completion gate passes;
- critic approves whole-demand evidence.

## Immediate One-Week Cut

If the team wants the highest-leverage first sprint, do this:

1. Wire `on_error` and `on_stream_event` in `MiddlewareChain`.
2. Add `CallLimitMiddleware` and `LoopDetectionMiddleware`.
3. Turn middleware `CONFIRM` into a persisted approval request with `approval_id`.
4. Add approval resume regression tests.
5. Add failure-pattern query primitives over existing `agent_traces`.

This avoids a broad rewrite while attacking the highest-risk runtime gaps.

## Sources

- OpenAI Harness Engineering: https://openai.com/index/harness-engineering/
- OpenAI Agents SDK guardrails and human review: https://developers.openai.com/api/docs/guides/agents/guardrails-approvals
- OpenAI tool search: https://developers.openai.com/api/docs/guides/tools-tool-search
- OpenAI prompt caching: https://developers.openai.com/api/docs/guides/prompt-caching
- LangChain Deep Agents harness engineering: https://www.langchain.com/blog/improving-deep-agents-with-harness-engineering
- LangGraph overview: https://docs.langchain.com/oss/python/langgraph/overview
- LangGraph persistence: https://docs.langchain.com/oss/python/langgraph/persistence
- LangGraph interrupts: https://docs.langchain.com/oss/python/langgraph/interrupts
- Anthropic context engineering: https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- Manus context engineering: https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus
- OpenTelemetry GenAI attributes: https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/
- Langfuse evaluation overview: https://langfuse.com/docs/evaluation/overview
- Temporal OpenAI Agents SDK integration: https://temporal.io/blog/announcing-openai-agents-sdk-integration
