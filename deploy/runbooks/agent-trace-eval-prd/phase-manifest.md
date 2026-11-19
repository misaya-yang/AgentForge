# Agent Trace Eval PRD Harness Phase Manifest

This is the compact index for coding agents. Load this file only when the target phase is unknown; otherwise start from `context-profile.json`, `loop-state.json`, and the active phase file.

## Grep Usage

Find a phase:

```bash
rg -n "PHASE_ID: ATE-" deploy/runbooks/agent-trace-eval-prd/phase-*.md
```

Find goal prompts:

```bash
rg -n "GOAL_PROMPT:" deploy/runbooks/agent-trace-eval-prd/phase-*.md
```

Find validation commands:

```bash
rg -n "VALIDATION_COMMANDS:" deploy/runbooks/agent-trace-eval-prd/phase-*.md
```

Find acceptance gates:

```bash
rg -n "ACCEPTANCE_GATES:" deploy/runbooks/agent-trace-eval-prd/phase-*.md
```

Validate harness structure:

```bash
python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --quality-score
```

Validate a completed phase claim:

```bash
python3 validate_harness_prd.py deploy/runbooks/agent-trace-eval-prd --strict --completion-gate --phase ATE-00 --quality-score
```

## Phase Index

| PHASE_ID | File | Depends On | Goal Target | Main Validation | Evidence Output |
| --- | --- | --- | --- | --- | --- |
| ATE-00 | `phase-00-baseline-trace-architecture.md` | none | Freeze repo-specific trace architecture, first-wave scope, and validation boundaries before implementation. | strict harness validator plus repo manifest discovery and git ignore proof | `reports/ate-00-baseline-trace-architecture-report.md` |
| ATE-01 | `phase-01-ai-assistant-trace-schema-and-api.md` | ATE-00 | Add the AI Assistant trace database contract and tenant-scoped Eval API surface. | backend lint plus eval API, tenant isolation, and OpenAPI contract tests | `reports/ate-01-ai-assistant-trace-schema-and-api-report.md` |
| ATE-02 | `phase-02-assistant-trace-capture.md` | ATE-01 | Persist AI Assistant trace evidence without adding user-visible agent latency. | assistant-service lint plus trace capture, latency guard, streaming contract, and isolation tests | `reports/ate-02-assistant-trace-capture-report.md` |
| ATE-03 | `phase-03-eval-console-ui.md` | ATE-02 | Add the first Eval console tab for AI Assistant trace review and scoring. | web lint, type check, browser route check, and open-source e2e smoke | `reports/ate-03-eval-console-ui-report.md` |
| ATE-04 | `phase-04-release-regression-and-handoff.md` | ATE-03 | Run release regression and hand off the LangGraph Proxy Trace and RAG Trace expansion contracts. | whole-demand regression across backend, latency guard, frontend, harness, and open-source gates | `reports/ate-04-release-regression-and-handoff-report.md` |

## Phase Report Index

| PHASE_ID | Required Actor Report | Required Critic Artifact |
| --- | --- | --- |
| ATE-00 | `reports/ate-00-baseline-trace-architecture-report.md` | `reports/ate-00-baseline-trace-architecture-critic.md` |
| ATE-01 | `reports/ate-01-ai-assistant-trace-schema-and-api-report.md` | `reports/ate-01-ai-assistant-trace-schema-and-api-critic.md` |
| ATE-02 | `reports/ate-02-assistant-trace-capture-report.md` | `reports/ate-02-assistant-trace-capture-critic.md` |
| ATE-03 | `reports/ate-03-eval-console-ui-report.md` | `reports/ate-03-eval-console-ui-critic.md` |
| ATE-04 | `reports/ate-04-release-regression-and-handoff-report.md` | `reports/ate-04-release-regression-and-handoff-critic.md` |

## Dependency Flow

```text
ATE-00 Baseline Trace Architecture
  -> ATE-01 AI Assistant Trace Schema and API
  -> ATE-02 Assistant Trace Capture
  -> ATE-03 Eval Console UI
  -> ATE-04 Release Regression and Handoff
```

## Validation Matrix

| PHASE_ID | Mutates Data | Needs Browser/UI | Needs Agent/LLM Eval | Needs Migration | Needs External Service | Release Blocking |
| --- | --- | --- | --- | --- | --- | --- |
| ATE-00 | no | no | design evidence only | no | no | no |
| ATE-01 | local development database schema only | no | score contract only | yes, additive migration | no | yes |
| ATE-02 | local development assistant trace rows only | no | assistant trace persistence with non-blocking latency guard | no new schema unless ATE-01 reports a blocker | no | yes |
| ATE-03 | no server mutation beyond score API calls in seeded test data | yes, `/eval` | trace review and scoring UI | no | no | yes |
| ATE-04 | no production mutation | yes, regression screenshots when UI exists | full AI Assistant trace regression | no new migration | no | yes |

## Risk Matrix

| PHASE_ID | Primary Risk | Stop Condition |
| --- | --- | --- |
| ATE-00 | first-wave AI Assistant scope drifts into LangGraph Proxy or RAG implementation | stop if phase boundaries cannot keep LangGraph Proxy and RAG Trace read-only |
| ATE-01 | trace tables expose cross-tenant data or store unredacted sensitive content | stop if tenant filters, redaction rules, or additive rollback are not testable |
| ATE-02 | trace capture delays the agent response path or duplicates terminal events | stop if latency guard tests fail, streaming-first tests fail, or trace writes cannot be retried without duplicate rows |
| ATE-03 | Eval console leaks prompt content or blocks existing assistant navigation | stop if auth gating, redaction display, or browser route checks fail |
| ATE-04 | release claim misses cross-layer regression or leaves expansion contracts ambiguous | stop if whole-demand regression cannot prove AI Assistant trace flow from chat to Eval UI |

## Runtime Artifacts

| Artifact | Path | Agent Rule |
| --- | --- | --- |
| Context Profile | `deploy/runbooks/agent-trace-eval-prd/context-profile.json` | Load first; it defines progressive disclosure and role budgets. |
| Loop State | `deploy/runbooks/agent-trace-eval-prd/loop-state.json` | Load first; keep active phase, feature, iteration, status, and next action current. |
| Loop Contract | `deploy/runbooks/agent-trace-eval-prd/loop-contract.json` | Open when loop semantics are unclear. |
| Feature Oracle | `deploy/runbooks/agent-trace-eval-prd/feature-oracle.json` | Inspect only the active feature item unless repairing oracle coverage. |
| Progress Log | `deploy/runbooks/agent-trace-eval-prd/progress-log.md` | Inspect recent entries when blocker or status history is unclear. |
| Agent Handoff | `deploy/runbooks/agent-trace-eval-prd/agent-handoff.md` | Open when the next action or role handoff is unclear. |
| Continuity Ledger | `deploy/runbooks/agent-trace-eval-prd/continuity-ledger.md` | Open dependency rows for the active phase and update handoff facts after validation. |
| Next Window Prompt | `deploy/runbooks/agent-trace-eval-prd/next-window-prompt.md` | Open only when preparing a fresh context window. |

## Agent Role Handoffs

- Planner role: convert product intent and industry research into phase contracts, feature-oracle cases, and risk gates.
- Generator role: execute one phase and one feature item, update evidence files, and hand off to evaluation.
- Critic role: use an independent subagent or fresh context to review actor output against acceptance gates, validation evidence, minimal-change scope, and regression impact.
- A phase completion claim is invalid until a separate critic artifact includes `Critic Verdict`.

## Delivery Quality Gates

- Each phase is independently executable and verifiable.
- Each phase records inherited evidence, dependency status, and the exact next unlock.
- Phase 1 implementation is limited to AI Assistant traces; LangGraph Proxy Trace and RAG Trace receive contracts and later-phase handoff only.
- Trace persistence must not delay AI Assistant first token, stream order, final response, or run status updates.
- Each phase records validation evidence, independent critic evidence, and a minimal-change note.
- The terminal phase runs whole-demand regression across completed feature-oracle items.
- Runtime files must let a fresh agent resume after context compaction.
- Cold start uses `context-profile.json`; agents do not load the full runbook folder by default.
- `--strict` proves structure readiness; `--completion-gate` proves phase completion.

## Goal Setup Templates

Use the exact phase file `GOAL_PROMPT` when creating an agent goal. If a phase has dependencies, execute it only after dependency acceptance gates pass or are explicitly waived in the previous phase report.

Example:

```text
Complete ATE-00 Baseline Trace Architecture for `.` by following `deploy/runbooks/agent-trace-eval-prd/phase-00-baseline-trace-architecture.md`; work on feature-oracle item ATE-F001; stay inside named edit boundaries; finish only after validation, regression, compliance, rollback, evidence, minimal-change, critic, and acceptance gates pass or blockers are documented.
```

## Shared Agent Rules

- Use the exact phase `GOAL_PROMPT` when starting a goal.
- Open only `READ_FIRST` and `PRIMARY_CONTEXT` before planning.
- Make the smallest requirement-satisfying change.
- Expand edit scope only when a blocker is documented in the phase report.
- Write test evidence, independent critic evidence, and the phase report before moving on.
- Run whole-demand regression in ATE-04 before claiming the PRD demand is complete.

## External Inputs Checklist

- No production credentials are required for planning or local implementation.
- No deployment is required for ATE-00 through ATE-04.
- Browser evidence is required starting in ATE-03.
- Any future external LangSmith, Langfuse, Phoenix, or MLflow integration requires a separate approval and a new feature-oracle item.
