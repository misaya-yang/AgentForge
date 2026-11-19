# Assistant Hermes OpenClaw Runtime PRD Continuity Ledger

**Created:** 2026-06-30

**Adapted On Main:** 2026-07-01

**Harness Folder:** `deploy/runbooks/assistant-hermes-runtime-prd`

---

## Purpose

This file preserves cross-phase continuity for long-running agents. Treat it as the bridge between product intent, code facts, execution evidence, and the next agent's starting point.

## Phase Continuity Chain

| Phase | Feature | Depends On | Unlocks | Handoff Boundary | Required Writeback |
| --- | --- | --- | --- | --- | --- |
| AHR-00 | AHR-F001 | none | AHR-01 | phase report plus handoff notes | source-packet and continuity-ledger code facts |
| AHR-01 | AHR-F002 | AHR-00 | AHR-02 | phase report plus handoff notes | source-packet and continuity-ledger code facts |
| AHR-02 | AHR-F003 | AHR-01 | AHR-03 | phase report plus handoff notes | source-packet and continuity-ledger code facts |
| AHR-03 | AHR-F004 | AHR-02 | AHR-04 | phase report plus handoff notes | source-packet and continuity-ledger code facts |
| AHR-04 | AHR-F005 | AHR-03 | AHR-05 | phase report plus handoff notes | source-packet and continuity-ledger code facts |
| AHR-05 | AHR-F006 | AHR-04 | none | phase report plus handoff notes | source-packet and continuity-ledger code facts |

## Interface Boundary Ledger

| Boundary | Current Fact | Source | Last Verified | Owner Phase |
| --- | --- | --- | --- | --- |
| Product scope | Keep AI--Platfform as the enterprise Assistant runtime/eval platform; use Hermes for agent-native runtime completeness and OpenClaw for runtime protocol/ops discipline. | `product-prd.md` | 2026-06-30 docs review | AHR-00 |
| Hermes evidence boundary | Hermes product direction comes from `downloads/Hermes_Agent_技术分析与AI平台对标报告.docx`. No local Hermes checkout was found, but user approved remote source lookup on 2026-07-01; official `NousResearch/hermes-agent` HEAD `729bbb7a309a3d13d8cc7d1cd2fbab79e7d969f7` and pinned raw files were read as remote source anchors. Do not vendor, import, execute, or depend on Hermes code. | `product-prd.md`, `source-packet.md`, `git ls-remote`, pinned GitHub raw source | 2026-07-01 AHR-00 source recheck | AHR-00 |
| Current main path boundary | Current `main` path probe confirmed Assistant/eval/runtime files named in the source packet exist, including `apps/assistant-service/src/assistant_service/core/trace_writer.py` and `web/src/pages/eval`. Focused `rg` verified anchor symbols for `AgentLoop`, `AssistantTraceWriter`, `AssistantExecutionGateway`, approval/checkpoint handling, Docker/gVisor code executor references, `safe_fetch`, and Eval dataset/evaluator/experiment APIs. | local path probe, focused `rg`, `source-packet.md` | 2026-07-01 AHR-00 source recheck | AHR-00 |
| OpenClaw path boundary | Verified OpenClaw paths are `src/agents/system-prompt.ts`, `src/context-engine/types.ts`, `packages/memory-host-sdk/src/host/session-files.ts`, `src/hooks/bundled/session-memory/handler.ts`, `extensions/memory-core/index.ts`, `src/plugins/registry.ts`, `docs/gateway/doctor.md`, and `SECURITY.md`. The expected whitepaper path and stale `src/memory/session-files.ts` path must not be cited as current evidence. | OpenClaw path probe, `source-packet.md` | 2026-07-01 AHR-00 source recheck | AHR-00 |
| Assistant runtime entrypoints | Candidate AI surfaces: `agent_loop.py`, `assistant_service.py`, `api/routes/*`, `execution_gateway.py`, stream event mapping, checkpoint routes. | `source-packet.md` | 2026-06-30 docs review | AHR-01 |
| Run/session/turn contract boundary | AHR-01 added `assistant-turn-contract/v1` context snapshots and terminal envelopes across streaming, non-stream, trace metadata, approval pending, cancellation, and completion paths. Public payloads remain additive. | `turn_contract.py`, `agent_loop.py`, `assistant_service.py`, `trace_writer.py`, AHR-01 report | 2026-07-01 AHR-01 implementation | AHR-01 |
| Context compiler boundary | Assistant now records runtime facts used for model input: policy, memory counts, workspace/file count, selected tool names/schema hashes, bootstrap counts, provider/model, trace ids, and surface state. Future phases must extend this shape rather than creating a competing metadata contract. | `source-packet.md#AHR-01 Implementation Evidence (2026-07-01)` | 2026-07-01 AHR-01 implementation | AHR-01 |
| Memory/context boundary | AHR-02 added `assistant-memory-lifecycle/v1`, completed-turn-only durable sync, bounded/deduped source-store write results, runtime memory provenance, and compaction lineage. Durable memory, session transcript, checkpoint summary, trace, and retrieval index remain separate state layers. | `source-packet.md#AHR-02 Implementation Evidence (2026-07-01)`, AHR-02 report | 2026-07-01 AHR-02 implementation | AHR-02 |
| Workspace memory source boundary | AHR-02 implements workspace memory enumeration for `MEMORY.md`, `memory.md`, `memory/**/*.md`, optional extra paths confined under the workspace root, symlink skipping, and realpath de-duplication. | `source_store.py`, AHR-02 report | 2026-07-01 AHR-02 implementation | AHR-02 |
| Pre-compaction flush boundary | AHR-02 exposes pre-compaction lifecycle hook and flush output through `AssistantRuntimeAdapter.on_pre_compact()` and `context_compacted.pre_compaction_flush`; compaction lineage records parent/child hashes and generated summary provenance. It does not silently turn interrupted turns into long-term memory. | `runtime_adapter.py`, `agent_loop.py`, AHR-02 report | 2026-07-01 AHR-02 implementation | AHR-02 |
| Tool safety boundary | AHR-03 added tool governance metadata, fail-closed direct registry execution for medium/high-risk or confirmation-required tools, duplicate-registration denial by default, gateway-approved invocation metadata, approval DB-failure denial, audit summary redaction, MCP catalog/parameter sanitization, and configured sandbox runtime enforcement. | `source-packet.md#AHR-03 Implementation Evidence (2026-07-01)`, AHR-03 report | 2026-07-01 AHR-03 implementation | AHR-03 |
| Eval/observability boundary | AHR-04 added bounded `assistant-runtime-trajectory/v1` trace metadata, tool-safety span fields, dashboard `runtime_health`, trace-to-dataset expected trajectory metadata, runtime trajectory feedback cases, 16 golden regression cases, and Eval UI runtime cockpit/detail surfaces. | `source-packet.md#AHR-04 Implementation Evidence (2026-07-01)`, AHR-04 report | 2026-07-01 AHR-04 implementation | AHR-04 |
| Doctor/status boundary | Future Assistant Runtime Doctor is read-only, redacted, bounded, offline-first, and covers DB/Redis, imports, trace writer, eval outbox, memory index, tool registry, sandbox, safe_fetch, TS contract drift, export redaction, `runtime_trajectory`, dashboard `runtime_health`, tool-safety failures, and golden trajectory gate status. | `openclaw-synthesis.md`, `phase-05-operating-model-and-release-gate.md`, AHR-04 report | 2026-07-01 AHR-04 implementation | AHR-05 |
| Validation boundary | PRD validation is docs-only; implementation phases must record exact scoped commands and outputs before claiming pass. | phase contracts | 2026-06-30 docs review | AHR-05 |
| Handoff boundary | Do not unlock a dependent phase until report evidence, oracle evidence, progress log, and this ledger are updated. | phase report | 2026-06-30 docs review | all |
| Terminology invariant boundary | The stable meanings of run, session, thread, turn, trace, checkpoint, memory source, transcript, plugin, skill, tool, evaluator, and gate are recorded in `source-packet.md`; downstream phases must preserve these terms or document a compatibility note. | `source-packet.md#AHR-00 Baseline Evidence (2026-07-01)` | 2026-07-01 AHR-00 source recheck | all |
| Phase edit boundary | AHR-01 through AHR-05 edit surfaces and protected non-goals are frozen in the AHR-00 source matrix; future scope expansion must be recorded in the phase report before edits. | `source-packet.md#Implementation Boundaries` | 2026-07-01 AHR-00 source recheck | all |

## Decision Ledger

| Decision | Rationale | Applies To |
| --- | --- | --- |
| Do not copy Hermes architecture wholesale. | AI--Platfform already has stronger multi-tenant runtime, eval, approval, and DB boundaries. | all phases |
| Do not copy OpenClaw architecture wholesale. | OpenClaw is a single-operator runtime reference; AI--Platfform must keep its stronger multi-tenant and enterprise boundaries. | all phases |
| Treat Hermes local execution as a warning, not a pass condition. | Hermes docs state approvals/redaction/tool allowlists are not containment; OS/process sandbox is the real boundary. | AHR-03 |
| Treat OpenClaw doctor/status as a diagnostic reference, not an auto-repair mandate. | The first AI--Platfform doctor plan must be read-only and redacted; repair/migration requires a separate approval. | AHR-05 |
| Keep eval self-hosted and offline-first. | AI--Platfform's own dataset/evaluator/run/gate model is stronger than adopting external observability SaaS as a dependency. | AHR-04, AHR-05 |
| Keep runtime cockpit data internal and bounded. | AHR-04 records trajectory and health summaries in AI--Platfform trace/eval metadata without storing raw prompts, raw tool arguments, or adopting an external tracing SaaS as a hard dependency. | AHR-04, AHR-05 |
| Security gates are release-blocking. | If risky tools bypass ExecutionGateway, eval quality numbers are not trustworthy. | AHR-03 through AHR-05 |
| Deny risky direct tool execution unless gateway-approved. | Risky tools must use the audited AssistantExecutionGateway path; approval DB failure, missing approval, and failed approval consumption are denial states. | AHR-03 through AHR-05 |

## Code Summary Writeback Rules

- After inspecting code, summarize discovered files, services, routes, schemas, tests, and runtime commands back into `source-packet.md`.
- Record cross-phase interface decisions here before handing off, especially API contracts, shared state, data shape, UI route assumptions, eval criteria, and rollback boundaries.
- If a phase changes a boundary another phase depends on, update that dependent phase's report handoff and the relevant oracle item notes.
- If a second agent cannot identify the next concrete action from this file, `progress-log.md`, and `agent-handoff.md`, stop and write a blocker instead of guessing.

## Current Continuity Status

- Active phase: AHR-05 (terminal, passed).
- Active feature-oracle item: AHR-F006 (passing).
- Current decision: AHR-05 published the Assistant Runtime Operating Model runbook, implemented offline `make verify-assistant-runtime-dev` regression gate (5/5 groups, 162 tests + 16 golden cases), generated JSON/Markdown reports under `reports/assistant-runtime-regression/`, documented CI adoption policy, waiver policy, no-go thresholds, terminology invariants, and completed whole-demand regression across AHR-F001 through AHR-F006.
- Next action: No further phases. The requirement chain is complete. Future work should start from a new PRD or extension phase.
