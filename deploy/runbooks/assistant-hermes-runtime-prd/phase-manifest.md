# Assistant Hermes OpenClaw Runtime PRD Phase Manifest

This is the compact index for coding agents. Prefer this file plus the target phase file over loading the whole folder.

## Grep Usage

Find a phase:

```bash
rg -n "PHASE_ID: AHR-XX" deploy/runbooks/assistant-hermes-runtime-prd
```

Find source-backed product requirements:

```bash
rg -n "AHR-F00|Requirement|acceptance_gates|GOAL_PROMPT" deploy/runbooks/assistant-hermes-runtime-prd
```

Validate this PRD harness structure:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --quality-score
```

Validate a future phase completion claim:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-hermes-runtime-prd --strict --completion-gate --phase <PHASE_ID> --quality-score
```

## Phase Index

| PHASE_ID | File | Depends On | Goal Target | Main Validation | Evidence Output |
| --- | --- | --- | --- | --- | --- |
| AHR-00 | `phase-00-comparative-baseline-evidence.md` | none | Freeze AI--Platfform vs Hermes plus OpenClaw evidence, boundaries, terminology invariants, and validation commands before code edits. | harness strict validation plus source-path recheck | `reports/ahr-00-comparative-baseline-evidence-report.md` |
| AHR-01 | `phase-01-entry-session-and-turn-contract.md` | AHR-00 | Add an Assistant run/session/turn envelope, context compiler snapshot, and stream/non-stream terminal parity checks. | assistant API/runtime contract tests | `reports/ahr-01-entry-session-and-turn-contract-report.md` |
| AHR-02 | `phase-02-memory-context-and-compaction-lineage.md` | AHR-01 | Harden memory SOT, provider lifecycle, workspace source enumeration, transcript separation, completed-turn sync, pre-compaction flush, and compaction lineage. | memory/runtime adapter/checkpoint tests | `reports/ahr-02-memory-context-and-compaction-lineage-report.md` |
| AHR-03 | `phase-03-tool-permission-and-runtime-safety.md` | AHR-02 | Make risky tool execution fail closed and separate skill, plugin, connector, MCP, and internal tool governance. | gateway/tool/audit/security tests | `reports/ahr-03-tool-permission-and-runtime-safety-report.md` |
| AHR-04 | `phase-04-observability-eval-and-regression-cockpit.md` | AHR-03 | Connect runtime context/tool/memory trajectories to trace, eval, review, golden, and gate workflows. | eval API/service tests plus web lint/type-check if UI changes | `reports/ahr-04-observability-eval-and-regression-cockpit-report.md` |
| AHR-05 | `phase-05-operating-model-and-release-gate.md` | AHR-04 | Publish no-go thresholds, doctor/status plan, terminology invariants, report output, and a stable offline runtime regression gate plan. | verify-eval-dev, eval gate, strict full harness validation | `reports/ahr-05-operating-model-and-release-gate-report.md` |

## Feature Map

| Feature | Requirement | Phase | Release Risk |
| --- | --- | --- | --- |
| AHR-F001 | Source-backed comparison baseline with OpenClaw synthesis | AHR-00 | Wrong phase boundaries if evidence or terminology is stale. |
| AHR-F002 | Run/session/turn contract plus context compiler snapshot | AHR-01 | UI/eval/resume cannot explain why a turn stopped or what capabilities were actually available. |
| AHR-F003 | Memory lifecycle, workspace source indexing, transcript separation, and compaction lineage | AHR-02 | Long-term memory pollution, duplicate facts, transcript confusion, stale indexes, lost lineage. |
| AHR-F004 | Tool permission, plugin trust, and runtime safety | AHR-03 | Risky tools bypass approval, plugin text becomes trusted, or sensitive data leaks. |
| AHR-F005 | Eval and observability cockpit for runtime trajectories | AHR-04 | Runtime context/tool/memory failures stay invisible and cannot become regression cases. |
| AHR-F006 | Operating model, doctor/status, terminology invariants, and release gate | AHR-05 | Quality claims cannot enter local/CI gates repeatably or be diagnosed on a fresh Mac. |

## Phase Report Index

| PHASE_ID | Required Report |
| --- | --- |
| AHR-00 | `reports/ahr-00-comparative-baseline-evidence-report.md` |
| AHR-01 | `reports/ahr-01-entry-session-and-turn-contract-report.md` |
| AHR-02 | `reports/ahr-02-memory-context-and-compaction-lineage-report.md` |
| AHR-03 | `reports/ahr-03-tool-permission-and-runtime-safety-report.md` |
| AHR-04 | `reports/ahr-04-observability-eval-and-regression-cockpit-report.md` |
| AHR-05 | `reports/ahr-05-operating-model-and-release-gate-report.md` |

## Dependency Flow

```text
AHR-00 Comparative Baseline Evidence
  -> AHR-01 Entry Session And Turn Contract
  -> AHR-02 Memory Context And Compaction Lineage
  -> AHR-03 Tool Permission And Runtime Safety
  -> AHR-04 Observability Eval And Regression Cockpit
  -> AHR-05 Operating Model And Release Gate
```

## Validation Matrix

| PHASE_ID | Mutates Data | Needs Browser/UI | Needs Agent/LLM Eval | Needs Migration | Needs External Service | Release Blocking |
| --- | --- | --- | --- | --- | --- | --- |
| AHR-00 | no | no | no | no | no | no |
| AHR-01 | local/dev assistant rows only if tests require | no unless API UI changes | deterministic fixture eval only | additive only if schema needed | no | yes |
| AHR-02 | local/dev memory/checkpoint rows only | no | deterministic fixture eval only | additive only if lineage/profile schema needed | no | yes |
| AHR-03 | local/dev audit/approval rows only | no | no live LLM required | additive only if audit/tool metadata schema needed | no | yes |
| AHR-04 | local/dev eval rows only | yes if `/eval` changes | offline golden/evaluator gate | additive only if eval schema needed | no | yes |
| AHR-05 | no production data | no unless UI changed earlier | offline golden/runtime regression only | no new migration expected | no | yes |

## Risk Matrix

| PHASE_ID | Primary Risk | Stop Condition |
| --- | --- | --- |
| AHR-00 | Overclaiming comparison facts or using stale line-level evidence. | Stop if AI, Hermes, or OpenClaw paths cannot be re-verified locally. |
| AHR-01 | New envelope/context snapshot fields drift across stream/non-stream, trace, checkpoint, and UI. | Stop if terminal state or capability snapshot cannot be represented without breaking existing API. |
| AHR-02 | Memory writes race, duplicate, leak, sync failed/interrupted turns, or blur transcript and durable memory. | Stop if completed-turn-only and layer separation semantics cannot be proven by tests. |
| AHR-03 | High-risk tools bypass ExecutionGateway, plugin/connector text becomes trusted, or audit stores raw sensitive args. | Stop if any risky direct-execution or untrusted prompt-exposure path remains unguarded. |
| AHR-04 | Eval cockpit exports or displays sensitive runtime payloads or misses context/memory failure modes. | Stop if redaction/export or runtime trajectory regression tests fail. |
| AHR-05 | Runtime gate or doctor becomes flaky, mutating, or live-provider dependent. | Stop if gate is not offline, redacted, read-only, and repeatable by default. |

## Runtime Artifacts

| Artifact | Path | Agent Rule |
| --- | --- | --- |
| Product PRD | `deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md` | Human-readable product scope and comparison; read in planner/builder mode. |
| OpenClaw Synthesis | `deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md` | Open only when working on context compiler, memory layer, plugin/tool governance, doctor/status, or source matrix requirements. |
| Context Profile | `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json` | Load first; it defines hot path, role budgets, and deferred triggers. |
| Loop State | `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json` | Load first; keep active phase, feature, iteration, status, and next action current. |
| Loop Contract | `deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json` | Deferred; open only when loop semantics are unclear. |
| Feature Oracle | `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json` | Deferred; inspect only the selected feature item unless repairing oracle coverage. |
| Source Packet | `deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md` | Deferred; targeted lookup/writeback only after phase context says to. |
| Progress Log | `deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md` | Deferred; inspect recent entries only when blocker or status history is unclear. |
| Agent Handoff | `deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md` | Deferred; open only when next action or role handoff is unclear. |
| Continuity Ledger | `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md` | Deferred; open only dependency rows needed for target phase or writeback. |
| Next Window Prompt | `deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md` | Deferred; open only when preparing a fresh context window. |

## Agent Role Handoffs

- Planner role: keep `product-prd.md`, `source-packet.md`, and phase contracts aligned.
- Generator role: execute one phase/feature item, update evidence, and hand off to evaluation.
- Critic role: review actor output from files and runtime checks; reject missing tests, missing redaction gates, unbounded scope, or stale source facts.
- No phase completion claim is valid until independent critic evidence is recorded.

## Delivery Quality Gates

- Each phase is independently executable and verifiable.
- Each implementation phase records test evidence and independent critic evidence, or a blocker.
- Security-sensitive phases must include fail-closed tests, redaction tests, and explicit non-production boundaries.
- Terminal phase runs whole-demand regression across completed feature-oracle items.
- Runtime files must be sufficient for a fresh agent to resume after context compaction.
- `--strict` is structure readiness only; phase and full-demand completion require `--completion-gate`.

## Goal Setup Templates

Use the exact phase file `GOAL_PROMPT` when creating an agent goal. If a phase has dependencies, do not execute it until dependency acceptance gates are met or explicitly waived in the previous phase report.

Example:

```text
Complete AHR-00 Comparative Baseline Evidence for `.` by following `deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md`; stay inside named edit boundaries; finish only after code-summary writeback, continuity update, validation, regression, compliance, rollback, evidence, and acceptance gates pass or blockers are documented.
```

## Shared Agent Rules

- Open only `READ_FIRST` and bounded `PRIMARY_CONTEXT` before planning.
- Make the smallest requirement-satisfying change.
- Expand edit scope only when a blocker is documented.
- Do not deploy, migrate production, or read secrets.
- Write test evidence, independent critic evidence, and the phase report before moving on.
- Run whole-demand regression in the terminal phase or release gate.

## External Inputs Checklist

- No external inputs are guaranteed by this PRD.
- Record missing credentials, dashboards, Figma links, deployment access, migrations, provider approvals, and production data access before use.
