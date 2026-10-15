# OpenClaw Mechanism Synthesis

**Date:** 2026-06-30

**Scope:** Add OpenClaw as a second comparison source for the Assistant Runtime PRD. Keep Hermes as the agent-native shell reference, keep OpenClaw as the runtime protocol and operating model reference, and keep AI--Platfform as the product platform to improve.

This file is evidence and translation only. Do not import OpenClaw code or add OpenClaw as a dependency.

## Product Thesis Update

Hermes answers "how does an agent loop feel durable during local work?" OpenClaw answers "how do channels, sessions, context, tools, memory, plugins, sandbox, and health checks become a runtime protocol stack?" AI--Platfform should absorb both sets of ideas while preserving its stronger enterprise Assistant, approval, eval, and multi-tenant boundaries.

The target Assistant Runtime should be able to explain:

- Which runtime capabilities were truly available for this turn.
- Which session/thread/run lane owned the turn.
- Which trusted metadata versus untrusted user content entered context.
- Which memory layer supplied each injected fact.
- Which tool registry, approval gateway, sandbox, and redaction policy governed each action.
- Which trace, golden case, regression gate, or doctor check proves the behavior remains healthy.

## OpenClaw Mechanism Translation

| OpenClaw Mechanism | Evidence | AI--Platfform Translation | Target Phase |
| --- | --- | --- | --- |
| Runtime protocol stack | `whitepapers/openclaw-agent-engineering-whitepaper.md` describes channels, routing, sessions, prompt compilation, tools, memory, harness, and delivery as one constrained stack. | Treat Assistant as a protocol stack, not only a model call. The PRD must cover route, session, context compiler, tool policy, memory, trace, eval, and ops gates together. | AHR-00, AHR-01 |
| Runtime context compiler | `src/agents/system-prompt.ts:390-429` composes workspace, safety, skills, memory, docs, tool availability, and policy-filtered tool names; `src/agents/system-prompt.ts:704-724` emits a runtime line with agent, host, repo, model, shell, channel, capabilities, and thinking mode. | Add an Assistant Context Compiler contract. It must derive prompts from actual tool registry, memory sources, workspace scope, sandbox state, model/provider snapshot, and UI/channel surface. It must prevent prompt/tool capability drift. | AHR-01, AHR-03 |
| Bootstrap budget and truncation evidence | `docs/concepts/system-prompt.md` describes runtime-generated prompt inputs such as AGENTS/TOOLS/USER/BOOTSTRAP/MEMORY files; `src/agents/pi-embedded-helpers/bootstrap.ts` handles missing files, budget, and truncation. | Context compiler should record raw bytes, injected bytes, missing files, truncation reason, and context tier. Subtasks should default to minimal context rather than inheriting full workspace memory by accident. | AHR-01, AHR-02 |
| Context engine lifecycle | `src/context-engine/types.ts:62-150` defines bootstrap, ingest, ingestBatch, afterTurn, assemble, compact, and subagent lifecycle hooks. | Add a pluggable lifecycle boundary for Assistant context assembly and compaction even if the first implementation is DB-backed and internal. Tests must prove completed-turn-only sync and deterministic compaction metadata. | AHR-02 |
| Memory layer separation | `packages/memory-host-sdk/src/host/session-files.ts` stores and parses transcript-derived session entries; `src/hooks/bundled/session-memory/handler.ts` writes session summaries into `memory/` on new/reset with fallback transcript recovery. The initial `src/memory/session-files.ts` path was stale on 2026-07-01. | Separate Assistant memory into profile facts, project/workspace facts, session transcript, checkpoint summary, trace, and retrieval index. Do not let one layer silently masquerade as another. | AHR-02, AHR-04 |
| Skills versus plugins | `extensions/memory-core/index.ts` registers memory tools and CLI through a plugin; `src/plugins/registry.ts:575-607` allows plugins to register tools, hooks, HTTP routes, channels, providers, gateway methods, CLI, services, and context engines. | Split knowledge packages from runtime capabilities. Assistant skills/prompts are not authority to execute; plugins/connectors/tools require registry metadata, trust state, approval class, redaction policy, and owner phase. | AHR-03 |
| Prompt hook governance | `docs/tools/plugin.md` describes `allowPromptInjection`; `src/plugins/registry.ts` diagnoses or constrains hook-driven prompt changes when prompt injection is disabled. | Any plugin/connector/MCP/catalog text that can mutate prompt context must be explicit, allowlisted, bounded, sourced, ordered, and traceable with block reasons when rejected. | AHR-03, AHR-04 |
| Sandbox, tool policy, and elevated execution separation | `docs/gateway/sandbox-vs-tool-policy-vs-elevated.md` separates runtime location, policy, and elevation; `src/agents/pi-tools.policy.ts` merges policy from multiple sources. | ExecutionGateway decisions should expose effective policy, policy source, sandbox profile, approval/elevation state, and denial reason. Do not treat node/tool capability claims as authority. | AHR-03, AHR-05 |
| Canonical approval plan binding | `docs/gateway/protocol.md` requires approval requests to carry a canonical plan; `src/infra/exec-approvals.ts` binds plan fields such as argv, cwd, session, env hash, and mutable file operand. | Approval should be single-use and bound to tenant, user, run, session, tool, normalized args hash, cwd or resource scope, env preview hash, and mutable operands when present. | AHR-03 |
| Channel/session routing | `src/channels/plugins/types.plugin.ts:31-64` defines a channel plugin contract with auth, pairing, security, outbound, status, streaming, threading, messaging, and agent prompt adapters; `src/routing/session-key.ts:118-150` builds session keys from agent/channel/account/peer scope. | Assistant does not need external chat channels now, but it needs the same surface contract thinking: route identity, thread id, tenant/user scope, session isolation, stream/non-stream parity, and future channel-safe metadata. | AHR-01 |
| Trust boundary honesty | `SECURITY.md:90-110` says a gateway is not a multi-tenant adversarial boundary and plugins are trusted computing base; `SECURITY.md:153-186` treats the model as untrusted and boundaries as host/config/tool policy/sandbox/approvals. | Preserve AI--Platfform's stronger multi-tenant posture. Use OpenClaw's honesty as a checklist: never call approvals a containment boundary, never call plugin text trusted, and require sandbox/tool policy tests for risky actions. | AHR-03, AHR-05 |
| Doctor/status operations | `docs/gateway/doctor.md:59-83` lists health, skills, config normalization, state integrity, auth, sandbox, gateway runtime, channel status, service audit, port collision, and source install checks. | Add an Assistant Runtime Doctor. It should summarize DB/Redis, assistant-service, trace writer, eval outbox, memory index, tool registry, sandbox, safe fetch, schema drift, and redaction health without printing secrets. | AHR-05 |
| Source matrix discipline | `whitepapers/openclaw-agent-engineering-whitepaper.md` starts each section with source evidence, engineering interpretation, follow-up questions, and boundary reasoning. | Each phase report must include a source matrix, terminology invariants, before/after run trace, and critic verdict. This reduces PRD-to-code drift and makes future reviews faster. | AHR-00, AHR-05 |

## New PRD Requirements

### R6 - Runtime Context Compiler And Capability Truthfulness

Assistant prompts and model inputs should be compiled from runtime facts, not static wish lists.

Acceptance:

- Context compiler input includes tenant/user scope, run/session/thread ids, provider/model snapshot, tool registry snapshot, approved tool policy, sandbox state, memory source inventory, retrieval snippets, workspace scope, and UI/channel surface.
- Context compiler output records a bounded `context_snapshot_id` or hash that trace/eval can reference.
- Bootstrap inputs record raw bytes, injected bytes, missing files, truncation reasons, and context tier.
- Tool names and descriptions exposed to the model match the actual registry and permission policy for that turn.
- Trusted metadata is passed outside untrusted user-controlled content; retrieved snippets and external catalog text are marked untrusted.
- Tests fail if prompt-exposed capability and runtime-enabled capability drift.

### R7 - Layered Memory And Transcript Contract

Memory should be understandable enough for humans, tests, and eval to know why a fact appeared.

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

## Priority Changes To Existing Phase Plan

- AHR-00 must re-freeze the comparison as AI--Platfform versus Hermes plus OpenClaw, not only AI--Platfform versus Hermes.
- AHR-01 must add context compiler and capability truthfulness to the run/session/turn envelope.
- AHR-02 must treat memory as layers: profile, project, transcript, checkpoint, trace, and retrieval index.
- AHR-03 must split skills, plugins, connectors, and tools in the safety model.
- AHR-04 must make eval able to score context/tool/memory trajectory, not only final answer quality.
- AHR-05 must include doctor/status checks and terminology invariants before release gating.

## Non-Goals

- Do not add OpenClaw channels to AI--Platfform in this roadmap.
- Do not replace AI--Platfform memory storage with OpenClaw's file layout.
- Do not relax AI--Platfform multi-tenant assumptions to match OpenClaw's single-operator trust model.
- Do not make doctor auto-fix or migration behavior part of this docs-only update.
