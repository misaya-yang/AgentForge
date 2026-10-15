# Assistant Hermes OpenClaw Runtime PRD

**Date:** 2026-06-30

**Owner:** Product/engineering

**Purpose:** Compare AI--Platfform Assistant runtime with Hermes Agent and OpenClaw, then turn the useful Hermes runtime patterns and OpenClaw protocol/ops mechanisms into an additive AI--Platfform improvement roadmap.

---

## Harness Intent

Compare AI--Platfform Assistant runtime with Hermes Agent and OpenClaw. Turn useful Hermes runtime, memory, tool, observability, and eval patterns plus OpenClaw context compiler, layered memory, plugin/tool governance, and doctor/status mechanisms into an additive AI--Platfform improvement roadmap. The harness must preserve phase relatedness, code-summary writeback, and evidence handoff across long-running agent sessions.

## Coding Agent Loading Protocol

When assigned a phase goal:

1. Open `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json`.
2. Open `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json`.
3. Open the assigned target phase file. If the target is unknown, locate it with:

```bash
rg -n "PHASE_ID: <ID>|GOAL_PROMPT|VALIDATION_COMMANDS|ACCEPTANCE_GATES" deploy/runbooks/assistant-hermes-runtime-prd
```

4. Open only the target phase file and hot-path items allowed by `context-profile.json`.
5. Do not load the full docs folder, full `source-packet.md`, full `feature-oracle.json`, or prior reports unless the context profile trigger says to.
6. Create a plan before editing.
7. Treat `LIKELY_EDIT_PATHS` as the intended write boundary.
8. Complete validation, browser/runtime checks, regression scope, review, compliance gates, rollback notes, evidence output, and acceptance gates before claiming completion.
9. Summarize code facts and boundary decisions back into `source-packet.md` and `continuity-ledger.md` using targeted sections only.
10. Update `progress-log.md`, `agent-handoff.md`, the phase report, and only the relevant feature `status`/`evidence` fields in `feature-oracle.json`.
11. Run `--strict --completion-gate --phase <PHASE_ID>` before declaring a phase complete or unlocked.
12. Move to the next phase only after dependency gates are met or explicitly waived in a report.

## Long-Running Runtime Protocol

Each fresh session must start from the smallest durable context packet instead of hidden chat context or pre-compaction memory:

- Read `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json` first and follow its role-specific load budget.
- Read recent `deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md` entries only when the active blocker or status is unclear.
- Follow `deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json`: observe, select, execute, verify, record, then decide whether to continue or stop.
- Update `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json` when the active phase, feature, iteration, decision, or blocker changes.
- Update `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md` when code facts, interfaces, contracts, or handoff boundaries change.
- Run the baseline or smoke check named by the target phase before adding new changes.
- Work on one phase and one feature-oracle item at a time.
- Make the smallest requirement-satisfying change and record any scope expansion in the phase report.
- Mark oracle items `passing` only when evidence points to an actor report with `Status: passed` and an independent critic artifact with `Critic Verdict: approved` or `waived`.
- Leave the repo in a clean, restartable state or document the blocker in the phase report.

## Runtime Artifacts

| Artifact | Purpose |
| --- | --- |
| `deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md` | Human-readable Assistant vs Hermes plus OpenClaw product PRD, comparison, requirements, security gates, and validation strategy. |
| `deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md` | OpenClaw mechanism translation for context compiler, memory layers, plugin/tool governance, doctor/status, and evidence discipline. |
| `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json` | Progressive disclosure budget, hot-path files, role load profiles, and deferred triggers. |
| `deploy/runbooks/assistant-hermes-runtime-prd/loop-contract.json` | The control loop: observe, select, execute, verify, record, decide. |
| `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json` | Current phase, feature, iteration, status, last decision, and next action. |
| `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json` | End-to-end feature/test oracle. Agents may update status and evidence, not delete cases. |
| `deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md` | Chronological progress, current blocker, and clean-state notes for the next session. |
| `deploy/runbooks/assistant-hermes-runtime-prd/agent-handoff.md` | File-based planner/generator/critic handoff packet. |
| `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md` | Cross-phase continuity, code summary writeback, and interface boundary ledger. |
| `deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md` | Copy-ready prompt for starting the next agent window. |

## Source Packet

- `product-prd.md` is the human-readable product PRD and comparison summary.
- `source-packet.md` is the code/source evidence packet for agents. It records AI--Platfform paths, Hermes comparison paths, OpenClaw comparison paths, subagent findings, risk gates, and source-trust rules.
- `openclaw-synthesis.md` is the mechanism translation file. Open it when working on context compiler, memory layer separation, plugin/tool governance, doctor/status, or source matrix discipline.
- `feature-oracle.json` maps each product requirement to one executable phase and durable acceptance evidence.

## Current System Shape

AI--Platfform already has stronger product-platform primitives than Hermes in several areas: Assistant service runtime, ExecutionGateway approvals, bounded checkpoints, Docker/gVisor code execution, safe URL fetching, trace/eval schema, golden regression, and `/eval` cockpit.

Hermes is stronger as an agent-native runtime shell: CLI/gateway/TUI/ACP entrypoints, provider transport breadth, MEMORY.md and USER.md semantics, completed-turn memory sync, compaction lineage, SQLite transcript search, observer hooks, trajectory export, and a runtime-focused test harness.

OpenClaw is stronger as a runtime protocol and operating model reference: runtime context compiler, channel/session routing, workspace memory versus transcript separation, context engine lifecycle, plugin/tool governance, prompt hook policy, sandbox/tool-policy separation, canonical approval plans, and doctor/status checks.

This PRD keeps AI--Platfform as the platform and uses Hermes as a reference for missing runtime completeness and OpenClaw as a reference for runtime discipline: run/session/turn envelope, context compiler, layered memory, tool safety fail-closed behavior, observer hooks, runtime regression gates, and doctor/status checks.

## Assumptions and Decisions

- The phase order is `AHR-00` through `AHR-05`; implementation phases remain blocked until `AHR-00` re-verifies current code facts.
- Changes should be additive. Existing Assistant/Eval API shapes and DB meanings must be preserved unless a phase explicitly adds a compatibility note.
- Hermes and OpenClaw are comparison sources only; no Hermes or OpenClaw code is imported.
- Harness runtime files, Assistant durable memory, session transcript, trace, checkpoint summary, and pre-compaction flush output are separate state layers. Do not mix them in code summaries or acceptance evidence.
- Eval and regression remain offline-first for CI; real provider/judge runs require explicit later approval.
- Any risky tool execution path that bypasses ExecutionGateway or equivalent approval gateway is release-blocking.

## Phase Order

| Phase | Name | Core Outcome | Report |
| --- | --- | --- | --- |
| Phase 00 | Comparative Baseline Evidence | Code facts, validation commands, terminology invariants, and interface boundaries are written back for Assistant Hermes OpenClaw Runtime PRD. | `reports/ahr-00-comparative-baseline-evidence-report.md` |
| Phase 01 | Entry Session And Turn Contract | Entry Session And Turn Contract is executed against the inherited code summary and produces evidence for the next phase. | `reports/ahr-01-entry-session-and-turn-contract-report.md` |
| Phase 02 | Memory Context And Compaction Lineage | Memory Context And Compaction Lineage is executed against the inherited code summary and produces evidence for the next phase. | `reports/ahr-02-memory-context-and-compaction-lineage-report.md` |
| Phase 03 | Tool Permission And Runtime Safety | Tool Permission And Runtime Safety is executed against the inherited code summary and produces evidence for the next phase. | `reports/ahr-03-tool-permission-and-runtime-safety-report.md` |
| Phase 04 | Observability Eval And Regression Cockpit | Observability Eval And Regression Cockpit is executed against the inherited code summary and produces evidence for the next phase. | `reports/ahr-04-observability-eval-and-regression-cockpit-report.md` |
| Phase 05 | Operating Model And Release Gate | Operating Model And Release Gate is executed against the inherited code summary and produces evidence for the next phase. | `reports/ahr-05-operating-model-and-release-gate-report.md` |

## Roadmap Cohesion

The phase chain is `AHR-00 Comparative Baseline Evidence
  -> AHR-01 Entry Session And Turn Contract
  -> AHR-02 Memory Context And Compaction Lineage
  -> AHR-03 Tool Permission And Runtime Safety
  -> AHR-04 Observability Eval And Regression Cockpit
  -> AHR-05 Operating Model And Release Gate`. Each phase must inherit prior report evidence, preserve code/interface boundaries in the continuity ledger, and update the next handoff before unlocking dependent work. The terminal phase or release gate must run whole-demand regression across completed feature-oracle items before the full requirement is considered done.

## Delivery Quality Gates

- Every phase must be executable and verifiable on its own.
- Every phase must inherit prior evidence and record what it unlocks next.
- Every implementation must include test evidence and independent critic evidence or an explicit blocker.
- The terminal phase or release gate must run whole-demand regression across completed feature-oracle items.
- Runtime files must be current enough for a fresh agent to resume after context compaction.
- `--strict` is structure readiness, not completion proof; phase or full-demand completion requires `--completion-gate`.

## New Window Prompt

Use `deploy/runbooks/assistant-hermes-runtime-prd/next-window-prompt.md` when starting a new Codex, Claude Code, or compatible agent window. Prefer the exact target phase `GOAL_PROMPT` from the phase file when assigning implementation work.

## Shared Harness Rules

- Stay inside phase boundaries.
- Make the smallest requirement-satisfying change.
- Plan before editing.
- Do not claim completion without durable test, review, and regression evidence.
- Document blockers and user waivers explicitly.
- Keep runtime files current enough for recovery after context compaction.

## Global Non-Goals

- Do not deploy.
- Do not mutate production data.
- Do not expand beyond the named phase chain without updating the manifest and continuity ledger.

## Global Compliance Gates

- Do not expose secrets.
- Do not perform destructive commands.
- Document approval before external service, migration, deployment, or production data changes.

## Standard Verification Commands

Docs-only PRD validation:

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

These future commands are not claimed as passed by this PRD. Each implementation phase must record the exact command output in its phase report.

## Required Browser or Runtime Checks

No browser route is changed by this PRD. A future UI phase must verify `/eval` desktop and mobile with runtime summary, trace detail, review/golden actions, gate result state, and redaction/export behavior.

## External Inputs and Approvals

- No external inputs are approved by this PRD.
- Any credential, dashboard, Figma file, deployment target, DNS/provider change, or migration approval must be added to the source packet before use.
