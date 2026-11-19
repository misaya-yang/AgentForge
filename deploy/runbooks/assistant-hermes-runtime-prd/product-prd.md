# Assistant Runtime vs Hermes And OpenClaw Product PRD

**Date:** 2026-07-01

**Baseline:** seeded from `origin/dev_06-30`, adapted on current `main`

**Primary repo:** `the project root`

**Comparison sources:** `downloads/Hermes_Agent_技术分析与AI平台对标报告.docx`, `openclaw source`

## Executive Summary

AI--Platfform should not copy Hermes Agent or OpenClaw wholesale. The strongest product direction is to keep AI--Platfform's enterprise runtime foundation, eval platform, DB-backed memory, approval gateway, trace schema, and golden regression gate, then absorb the Hermes patterns that make an agent feel durable in real work and the OpenClaw patterns that make an agent runtime feel like a controlled protocol stack.

Hermes is stronger as an agent-native shell. It has mature CLI/gateway/TUI/ACP entrypoints, local session storage, MEMORY.md and USER.md semantics, provider lifecycle hooks, trajectory capture, optional observability plugins, and a pragmatic long-task recovery model. OpenClaw is stronger as a runtime protocol and operating model reference. It treats channels, routing, sessions, context compilation, tool governance, memory, plugins, sandbox, delivery, and doctor/status checks as one constrained runtime stack. AI--Platfform is stronger as a product platform. It already has multi-tenant Assistant service boundaries, ExecutionGateway approvals, checkpointing, Docker/gVisor code execution, safe fetch primitives, trace/eval/golden abstractions, Eval Console, and CI-oriented gates.

The product goal is therefore:

> Make AI--Platfform Assistant feel as operationally complete as Hermes and as runtime-disciplined as OpenClaw while preserving AI--Platfform's stronger enterprise safety, eval, and multi-tenant boundaries.

This PRD is intentionally additive. It does not require production migration, deployment, external SaaS, or changing existing API meanings. It creates a phase harness for future implementation so another agent can continue without hidden chat context.

## Product Framing

### Target Users

- Platform engineers who need the Assistant runtime to be debuggable, replayable, and safe under multi-tenant service boundaries.
- Product and support operators who need to understand why an Assistant run stopped, asked for approval, remembered a fact, skipped memory sync, or failed a tool call.
- Eval and quality reviewers who need to turn failed traces into golden cases, regression cases, or review tasks without leaking private prompts, tool arguments, or credentials.
- Future adapter owners for RAG, LangGraph proxy, MCP, skills, plugins, and connectors who need one canonical runtime contract instead of feature-specific trace or tool metadata.

### User Problems

- Long-running Assistant tasks can be hard to explain after the fact: the product should expose `run/session/thread/turn/trace/checkpoint` as consistent concepts.
- Context, memory, retrieved snippets, tool schemas, and prompt-exposed capability can drift from the actual runtime policy; this makes eval results hard to trust.
- Risky tool execution, plugin/connector text, MCP metadata, and code execution require a productized governance surface, not one-off prompt warnings.
- Existing eval is strong for trace and golden scoring, but runtime health signals such as context snapshot, memory sync, approval state, sandbox state, and tool policy are not yet one product workflow.

### MVP Slice

The first shippable slice is not "all Hermes/OpenClaw features." It is:

1. AHR-00 freezes source facts, terminology, gaps, and no-go gates.
2. AHR-01 adds the Assistant run/session/turn envelope and context snapshot contract.
3. AHR-03 closes any risky direct tool execution path that bypasses `ExecutionGateway`.
4. AHR-04 makes trace/eval expose the runtime trajectory needed to debug and score those decisions.

AHR-02 memory lineage and AHR-05 doctor/status then turn the MVP into an operational system. This order preserves safety and observability before expanding memory behavior.

### Success Metrics

- Debuggability: a reviewer can identify a failed run's exit reason, pending approval, checkpoint, trace id, model/provider snapshot, and tool policy from one trace or run detail.
- Safety: medium/high-risk tools cannot execute without gateway policy, approval binding, sandbox decision, and redacted audit evidence.
- Eval usefulness: failed runtime cases can be promoted into golden or review workflows with bounded, redacted payloads.
- Regression confidence: offline gates cover stream/non-stream parity, approval denial/resume, memory skipped-on-interrupt, sandbox unavailable, safe fetch, and trace redaction.
- Operator readiness: doctor/status output is read-only, bounded, redacted, and actionable without requiring provider keys or production access.

## Source Evidence

AI--Platfform evidence:

- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`: Assistant loop config, middleware chain, memory/context injection, trace writer boundaries, tool loop behavior, checkpoint writes.
- `apps/assistant-service/src/assistant_service/core/gateway/execution_gateway.py`: policy/sandbox decisions, approval creation, argument hash binding, checkpoint storage, resume preparation.
- `apps/assistant-service/src/assistant_service/core/runtime/memory/source_store.py`: markdown memory source-of-truth storage by tenant/user.
- `apps/assistant-service/src/assistant_service/core/runtime/compat/runtime_adapter.py`: request-time memory source loading, indexing, and retrieval.
- `apps/assistant-service/src/assistant_service/core/agent/middlewares/runtime_memory.py`: retrieved memory sanitization before context injection.
- `apps/assistant-service/src/assistant_service/core/code_executor.py`: Docker/gVisor, no-network code execution; no host fallback when Docker is unavailable.
- `packages/ai-gateway-core/src/ai_gateway_core/security/safe_fetch.py`: SSRF-resistant URL fetch primitive with private IP and redirect checks.
- `src/api/v1/eval.py`, `src/services/eval/golden.py`, `scripts/eval_golden.py`, `tests/fixtures/eval/golden/assistant_regression_v1.jsonl`: eval dashboard, datasets, evaluators, experiment runs, gates, and golden regression source.
- `deploy/runbooks/assistant-runtime-optimization/`: existing Assistant runtime optimization harness, reports, operating model, and completion gates.

Hermes evidence:

- `downloads/Hermes_Agent_技术分析与AI平台对标报告.docx`: local technical report used as the Hermes synthesis source for Context Compiler, harness engine, prompt layering, memory management, session isolation, tool governance, skills, safety, and observability recommendations.
- The report cites Hermes Agent v0.11.0 public materials and describes `AIAgent`, cached system prompt plus ephemeral overlays, MEMORY.md/USER.md bounded memory, SQLite session state, provider lifecycle hooks, tool registry/toolsets/approval, observer hooks, trajectory export, and local-execution security caveats.
- No local Hermes source checkout was found during the 2026-07-01 adaptation pass. During AHR-00 execution the user approved remote source lookup; official `NousResearch/hermes-agent` HEAD `729bbb7a309a3d13d8cc7d1cd2fbab79e7d969f7` and pinned raw files were verified as read-only comparison source anchors. Do not vendor, import, or execute Hermes code.

OpenClaw evidence:

- `openclaw source/whitepapers/openclaw-agent-engineering-whitepaper.md`: expected path from the initial draft, but this path was missing during AHR-00 source recheck on 2026-07-01. Use verified code/docs paths and `openclaw-synthesis.md` instead until the whitepaper is relocated.
- `openclaw source/src/agents/system-prompt.ts`: runtime prompt compilation from actual tools, skills, memory, docs, workspace, sandbox, messaging, and runtime line.
- `openclaw source/src/context-engine/types.ts`: context engine lifecycle with bootstrap, ingest, assemble, compact, afterTurn, and subagent lifecycle hooks.
- `openclaw source/packages/memory-host-sdk/src/host/session-files.ts` and `openclaw source/src/hooks/bundled/session-memory/handler.ts`: transcript parsing, redaction, line maps, reset/new memory summaries, and fallback transcript recovery.
- `openclaw source/src/plugins/registry.ts` and `openclaw source/extensions/memory-core/index.ts`: plugin API for registering tools, hooks, HTTP routes, channels, providers, CLI, services, and context engines.
- `openclaw source/src/channels/plugins/types.plugin.ts` and `openclaw source/src/routing/session-key.ts`: channel plugin and session-key contracts.
- `openclaw source/SECURITY.md`: single-operator trust model, plugin TCB, model-as-untrusted, and sandbox/tool-policy boundaries.
- `openclaw source/docs/gateway/doctor.md`: doctor/status model for health, state integrity, auth, sandbox, gateway runtime, channel status, service audit, and source install checks.
- `deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md`: translation of OpenClaw mechanisms into AI--Platfform Assistant requirements.

## Comparative Maturity

| Dimension | AI--Platfform | Hermes Agent | OpenClaw Signal | Product Decision |
| --- | --- | --- | --- | --- |
| Runtime entrypoints | Service API, assistant routes, gateway approvals, Eval UI | CLI, gateway API, TUI RPC, ACP, desktop bootstrap | Channels, routing, session key, runtime handle, delivery | Add a documented Assistant run/session/turn contract in AI--Platfform, not a new shell or external-channel bus. |
| Turn loop | AgentLoop with middleware, RAG/context, trace, checkpoints | Robust turn loop with provider modes, compression, tool repair, iteration fallback | Each attempt rebuilds tools from sender/channel/workspace/policy/sandbox | Add a structured turn envelope, exit reasons, and capability snapshot to AI--Platfform. |
| Context compiler | Runtime prompt/context injection exists in several places | Provider and memory lifecycle hooks influence context | System prompt is compiled from real tools, skills, memory, workspace, sandbox, messaging, and runtime line | Create an Assistant Context Compiler contract to prevent prompt/runtime capability drift. |
| Memory | Multi-tenant DB, memory sources, chunks, retrieval, PII filtering | MEMORY.md/USER.md, file locks, dedup, completed-turn sync, session lineage | Memory files, session transcripts, indexes, summaries, citations, and compaction are separate layers | Harden AI memory writes and add profile/lifecycle/layer semantics. |
| Tool safety | ExecutionGateway, approvals, code sandbox, safe_fetch, audit | Toolsets, registry shadow controls, approval floor, local execution warnings | Plugins are TCB; model is untrusted; policy/sandbox/approval are runtime boundaries | Make gateway the only path for risky tools; add registry fail-closed and plugin-trust gates. |
| Observability | Agent trace/eval schema, score, golden gate, Eval Console | Observer hooks, trajectory JSONL, NeMo Relay/Langfuse plugins | Source evidence, run trace, and health/status checks are first-class operating artifacts | Extend trace capture with runtime context/tool/memory trajectory while keeping self-hosted eval. |
| Eval platform | Dataset/evaluator/experiment/run/gate, `/eval`, CI gate | Engineering tests and trajectory export; not a first-class eval product | No direct equivalent; OpenClaw contributes runtime health and evidence discipline | AI--Platfform leads; use Hermes/OpenClaw trajectories and doctor checks as eval inputs. |
| Security posture | More productized: DB startup guard, safe fetch, Docker code executor | Honest about local backend risk and OS isolation requirement | Explicit single-operator trust model, plugin TCB, model-as-untrusted | Preserve AI--Platfform's stronger multi-tenant posture and adopt boundary-honesty checks. |
| Regression harness | `make verify-eval-dev`, `make eval-regression-gate`, harness reports | `scripts/run_tests.sh`, parallel pytest, runtime-focused tests | Doctor/status, source matrix, terminology invariants | Add assistant-runtime regression bundle and doctor summary focused on turn/session/tool/memory/recovery. |

## Product Scope

### In Scope

- Assistant-first runtime improvements in AI--Platfform.
- Additive documentation, phase contracts, tests, local gates, UI workbench improvements, and offline regression harnesses.
- Cross-repo comparison as product input only; Hermes and OpenClaw code are not imported.
- Self-hosted, offline-by-default eval and regression flow.
- Safety gates that assume LLM/tool/plugin content is untrusted.

### Out of Scope

- Production deployment.
- Production data mutation.
- External LangSmith, Langfuse, Phoenix, Braintrust, NeMo Relay, or SaaS dependency as a runtime requirement.
- Breaking existing Assistant, Eval, or database public contracts.
- Replacing AI--Platfform runtime with Hermes or OpenClaw architecture.
- Trusting local shell/code execution as a production-safe boundary.
- Adding external chat-channel support from OpenClaw in this roadmap.

## User Experience Requirements

The runtime platform should be inspectable from the existing enterprise console style, not a marketing surface.

- `/eval` remains the quality cockpit, with Assistant as the fully enabled first-class workflow.
- A future Assistant runtime view should expose run/session health, turn exits, pending approvals, tool denials, checkpoint/resume status, memory sync status, trace write failures, and regression gate status.
- Trace detail should show a compact trajectory summary: model calls, tool calls, approvals, retrieval, memory snippets, errors, and redaction status.
- Review workflows should allow a trace or failed turn to become a golden case or a runtime regression case.
- High-risk tool events should be visible with outcome, approval id, argument hash, sandbox profile, and redaction status, without raw secrets or raw tool args.

## Platform Requirements

### R1 - Entry, Session, and Turn Contract

AI--Platfform needs a durable Assistant run/session contract comparable in clarity to Hermes' `/v1/runs`, events, approval, stop, and session recovery surfaces.

Acceptance:

- Every Assistant run has a stable run id, session id, thread id when available, status, started/ended timestamps, model/provider snapshot, and tenant/user scope.
- Every turn returns or records a turn envelope with `exit_reason`, `interrupted`, `approval_pending`, `tool_error`, `max_iterations`, `checkpoint_id`, `trace_id`, token/cost summary when available, and redaction state.
- Stream and non-stream paths expose compatible events and terminal status.
- Stop/cancel/approval/resume paths are explicit and testable.
- Existing API shapes remain compatible; new fields are additive.

### R2 - Memory, Profile, and Compaction Lineage

AI--Platfform should keep DB-backed memory and retrieval, but adopt Hermes' clearer memory lifecycle.

Acceptance:

- Memory source writes are atomic or transactional, deduplicated, bounded, drift-aware, and threat-scanned.
- Stable user preferences are separated from daily/reflection/project facts, either as USER.md-equivalent profile documents or a first-class DB profile source.
- A MemoryProvider lifecycle exists for initialize, prefetch, sync_turn, on_session_end, on_session_switch, on_pre_compact, on_memory_write, and flush_pending.
- Only completed and non-interrupted turns can sync into long-term memory by default.
- Compaction/resume records parent/child lineage and summary provenance.
- Injected memory snippets carry source type, source path/id, score, recency, and an explicit untrusted-context marker.

### R3 - Tool Permission and Runtime Safety

ExecutionGateway should become the mandatory path for any medium/high-risk or confirmation-required tool.

Acceptance:

- Direct registry execution of medium/high/confirmation-required tools fails closed unless a test-only bypass is explicitly configured.
- Tool registration declares risk, permissions, sandbox profile, audit shape, and redaction policy.
- Duplicate or shadowed tool registration fails by default outside explicit trusted override flows.
- Approval is bound to tenant/user/run/tool/arguments_hash and is single-use.
- Denied, timed-out, or missing approval stops the risky action and produces a visible event.
- Code execution requires Docker/gVisor, no network by default, resource limits, no provider env, and no sensitive host mounts.
- URL fetch/callback callsites use `safe_fetch` or DNS-pinned equivalent primitives.
- MCP and external catalog text fields, including parameter descriptions, are sanitized, bounded, and marked untrusted.
- Tool audit logs do not store raw secrets or raw full arguments.

### R4 - Observability, Eval, and Regression Cockpit

AI--Platfform already leads Hermes on eval. The next step is to connect runtime quality and operational safety into one cockpit.

Acceptance:

- Trace capture includes consistent spans for run, model call, tool call, approval, memory retrieval, RAG retrieval, checkpoint/resume, and terminal status.
- Trace writer metrics expose dropped, timed out, failed, and redacted write counts.
- Eval Console shows latest Assistant baseline/candidate, pass rate, trajectory pass rate, critical failures, tool-safety failures, outbox failures, and judge/review pending counts.
- A trace can be promoted to review, golden, or failure-case workflow without leaking secrets.
- Export formats remain redacted and bounded.
- Golden regression includes runtime-specific cases for stop/resume, approval denial, tool argument mismatch, memory sync skipped on interrupted turn, and sandbox unavailable.

### R5 - Runtime Regression Harness

The repo needs a canonical command that proves the Assistant runtime still works in the ways that matter.

Acceptance:

- Add a future `make verify-assistant-runtime-dev` or extend `make verify-eval-dev` only after the command is stable.
- The command runs offline by default.
- The command covers stream/non-stream parity, turn envelope, approval/resume, direct registry bypass prevention, safe_fetch wiring, code sandbox failure mode, memory completed-turn sync, compaction lineage, and eval golden gate.
- Reports are written under `reports/assistant-runtime-regression/`.
- Failure is actionable: JSON and Markdown outputs identify failed case, expected behavior, actual behavior, and likely owner phase.

### R6 - Runtime Context Compiler And Capability Truthfulness

AI--Platfform should treat prompts and model inputs as runtime-compiled artifacts derived from real capability state.

Acceptance:

- Context compiler input includes tenant/user scope, run/session/thread ids, provider/model snapshot, tool registry snapshot, approved tool policy, sandbox state, memory source inventory, retrieval snippets, workspace scope, and UI/channel surface.
- Context compiler output records a bounded `context_snapshot_id` or hash that trace/eval can reference.
- Bootstrap/context inputs record raw bytes, injected bytes, missing files, truncation reasons, and context tier.
- Tool names and descriptions exposed to the model match the actual registry and permission policy for that turn.
- Trusted metadata is passed outside untrusted user-controlled content; retrieved snippets and external catalog text are marked untrusted.
- Tests fail if prompt-exposed capability and runtime-enabled capability drift.

### R7 - Layered Memory And Transcript Contract

AI--Platfform should keep its DB-backed model but make memory provenance as legible as file-backed systems.

Acceptance:

- Stable profile facts, project facts, user-authored memory sources, session transcripts, checkpoint summaries, trace spans, and vector/FTS indexes have separate ownership and provenance.
- Session transcript is authoritative for conversation replay; long-term memory is curated or lifecycle-synced; trace is observability; checkpoint is resume state.
- Memory sync is completed-turn-only by default and records skip reasons for cancelled, failed, interrupted, or approval-pending turns.
- Retrieved snippets include layer, source id/path, line or row locator when available, score, recency, and redaction state.
- Compaction preserves exact identifiers, pending asks, constraints, tool pairing, and parent/child lineage.

### R8 - Skill, Plugin, Connector, And Tool Governance

Assistant runtime should separate instruction packages from executable capability.

Acceptance:

- Skill-like knowledge packages can guide behavior but cannot grant execution rights.
- Plugin/connector/tool registration declares trust level, owner, permissions, risk, approval class, sandbox profile, audit shape, redaction policy, and prompt-exposure text.
- Prompt-exposure text from plugins, MCP, catalogs, webhooks, or external docs is bounded, sanitized, and marked untrusted unless it is code-owned metadata.
- Prompt mutation from plugins/connectors is opt-in, allowlisted, source-tagged, ordered, and rejected with a visible blocked reason when policy disallows it.
- Approval plans are canonicalized and bound to tenant, user, run, session, tool, normalized args hash, resource scope, env preview hash, and mutable operands when present.
- Duplicate or shadowed capability names fail closed unless a trusted override is explicit and audited.
- Registry snapshots are traceable to eval, golden cases, and doctor output.

### R9 - Assistant Runtime Doctor And Health Workbench

The platform should have a read-only health surface before deeper local-stack validation.

Acceptance:

- A future `make verify-assistant-runtime-dev` or CLI/API doctor mode checks Assistant runtime prerequisites without production mutation.
- Doctor output covers DB/Redis reachability, assistant-service import path, trace writer metrics, eval outbox, golden gate freshness, memory index health, tool registry drift, sandbox availability, safe_fetch wiring, frontend type contract drift, and redaction/export policy.
- Doctor output explains effective tool policy, policy source, sandbox availability, approval/elevation state, and blocked prompt-mutation reasons without exposing secrets.
- Output is pasteable, bounded, and redacted by default.
- Doctor checks distinguish warning, degraded, failed, blocked, and not configured.
- Any auto-repair or migration requires a separate explicit phase and approval.

### R10 - Phase Evidence Matrix And Terminology Invariants

The PRD harness should force implementation evidence to stay tied to source facts.

Acceptance:

- AHR-00 creates a source matrix covering AI--Platfform, Hermes, and OpenClaw evidence.
- Each phase report records exact terms and their meaning: run, session, thread, turn, trace, checkpoint, memory source, transcript, plugin, skill, tool, evaluator, gate.
- Each implementation phase records at least one before/after run or test trace when code changes runtime behavior.
- Independent critic review rejects unsupported product claims, stale paths, or missing redaction/safety evidence.

## Phase Roadmap

| Phase | Outcome | Main Implementation Surface | Required Evidence |
| --- | --- | --- | --- |
| AHR-00 Comparative Baseline Evidence | Freeze the AI vs Hermes plus OpenClaw source map, terminology invariants, and gap list. | Runbook docs only. | Updated source packet, continuity ledger, OpenClaw synthesis, and strict harness validation. |
| AHR-01 Entry Session And Turn Contract | Define and test the Assistant run/session/turn envelope plus context compiler snapshot. | Assistant API/routes, AgentLoop, stream event types, checkpoint/resume contracts, context compiler contract. | Unit/API tests for stream/non-stream, stop/cancel, approval pending, max-iteration, error exits, and capability truthfulness. |
| AHR-02 Memory Context And Compaction Lineage | Harden memory SOT and add layered memory/transcript/lifecycle semantics. | Runtime memory source store, runtime adapter, memory middleware, checkpoint metadata, context assembly. | Tests for atomic writes, dedup, completed-turn-only sync, snippet provenance, interrupted skip, compaction lineage, memory layer separation. |
| AHR-03 Tool Permission And Runtime Safety | Close risky direct-execution, plugin/connector trust, and audit/sanitization gaps. | ToolRegistry, ExecutionGateway, MCP manager/client, tool audit, safe_fetch callsites, registry metadata. | Tests for fail-closed registry execution, duplicate tool names, approval hash, redacted audit, MCP description sanitization, plugin trust metadata. |
| AHR-04 Observability Eval And Regression Cockpit | Connect runtime context/tool/memory trajectories to eval, review, golden, and gate workflows. | Trace writer, eval services/API, `/eval` UI, golden fixtures, regression reports. | Eval API tests, export redaction tests, UI type/lint, golden runtime cases, trace write metric tests, trajectory scoring. |
| AHR-05 Operating Model And Release Gate | Publish runtime operating model, doctor/status plan, terminology invariants, and stable dev gate. | Makefile, reports, runbook docs, CI draft/optional gate, health summary. | `make verify-eval-dev`, future runtime doctor/gate dry run, strict full harness validation, no-go thresholds. |

## Security Gates

- No production or shared mode may rely on local host command/code execution as containment.
- Any path that can run medium/high-risk tools without ExecutionGateway or equivalent approval gateway blocks release.
- Approval DB/audit DB unavailable in production-like mode means fail closed.
- Logs, trace, audit, checkpoint, export, and gate reports must not contain raw secrets, provider keys, raw cookies, raw headers, or raw full tool arguments.
- Docker/gVisor/sandbox unavailable means code execution fails closed.
- Untrusted MCP/webhook/browser content cannot enable local file, shell, or code execution toolsets.
- User denial or approval timeout must not be bypassed by alternate commands for the same effect.
- Prompt-exposed tools, skills, plugins, memory snippets, and surface capabilities must match the actual runtime snapshot for that turn.
- Doctor/status output must be redacted and read-only unless a later phase explicitly approves repair behavior.

## Validation Strategy

Local docs-only validation for this PRD:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --quality-score
```

Future implementation validation candidates:

```bash
make verify-eval-dev
make eval-regression-gate
uv run --package assistant-service pytest -q --no-cov tests/services/assistant tests/services/eval
uv run pytest -q --no-cov tests/api/test_eval_traces.py tests/api/test_eval_api_trace_tree.py
corepack pnpm@10.33.0 -C web lint
corepack pnpm@10.33.0 -C web type-check
```

Do not claim a runtime phase passed unless the phase report records the exact command output and an independent critic verdict.

## Open Risks

- AI--Platfform may already have some run/session contract fields in scattered routes; AHR-00 must freeze the actual API before AHR-01 edits code.
- Memory write hardening may require DB transaction patterns rather than file locks because AI--Platfform memory SOT is DB-backed. Preserve platform style.
- ToolRegistry fail-closed changes can break tests that currently call registry execution directly. Add an explicit test-only bypass rather than weakening production behavior.
- UI cockpit work should remain dense and operational; do not add marketing copy or one-off explanation panels.
- Runtime regression should enter `make verify-eval-dev` only after it is stable and offline.
