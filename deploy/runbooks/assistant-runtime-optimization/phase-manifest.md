# Assistant Runtime Optimization PRD Harness Phase Manifest

This is the compact index for coding agents. Prefer this file plus the target phase file over loading the whole folder.

## Grep Usage

Find a phase by id (grep header):

```bash
rg -n "PHASE_ID: ARO-XX" deploy/runbooks/assistant-runtime-optimization
```

Everything else (goal prompt, validation commands, gates, evidence) lives in each phase's single `## Machine Contract` JSON block. Read that block, or grep its keys:

```bash
rg -n '"prompt":|"command":|"acceptance_gates"' deploy/runbooks/assistant-runtime-optimization
```

Validate a completion claim:

```bash
python3 <skill-dir>/scripts/validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --phase <PHASE_ID> --quality-score
```

## Phase Index

| PHASE_ID | File | Depends On | Goal Target | Main Validation | Evidence Output |
| --- | --- | --- | --- | --- | --- |
| ARO-00 | `phase-00-baseline-runtime-and-industry-audit.md` | none | Prove the current runtime maturity judgment and freeze code/research evidence. | strict harness validation plus assistant/eval baseline tests | `reports/aro-00-baseline-runtime-and-industry-audit-report.md` |
| ARO-01 | `phase-01-middleware-harness-and-approval-completion.md` | ARO-00 | Complete lifecycle middleware hooks and persisted approval/resume semantics. | ruff, assistant runtime contracts, run/approval gateway contracts | `reports/aro-01-middleware-harness-and-approval-completion-report.md` |
| ARO-02 | `phase-02-trace-eval-feedback-loop.md` | ARO-01 | Turn trace families into redacted datasets, evaluator gates, and reviewed harness proposals. | ruff, Eval API/evaluator tests, optional web e2e | `reports/aro-02-trace-eval-feedback-loop-report.md` |
| ARO-03 | `phase-03-durable-long-task-runtime.md` | ARO-02 | Add AgentLoop checkpoint/resume and duplicate-side-effect prevention. | ruff, assistant checkpoint tests, run-state contract tests | `reports/aro-03-durable-long-task-runtime-report.md` |
| ARO-04 | `phase-04-performance-cost-and-context-optimization.md` | ARO-03 | Make cache/context/reasoning/tool optimization observable and eval-gated. | ruff, assistant context tests, eval quality gate | `reports/aro-04-performance-cost-and-context-optimization-report.md` |
| ARO-05 | `phase-05-release-regression-and-operating-model.md` | ARO-04 | Prove whole-demand regression and publish operating runbooks/SLO no-go thresholds. | verify-eval-dev, validate-example-config, full completion gate | `reports/aro-05-release-regression-and-operating-model-report.md` |

## Phase Report Index

| PHASE_ID | Required Report |
| --- | --- |
| ARO-00 | `reports/aro-00-baseline-runtime-and-industry-audit-report.md` |
| ARO-01 | `reports/aro-01-middleware-harness-and-approval-completion-report.md` |
| ARO-02 | `reports/aro-02-trace-eval-feedback-loop-report.md` |
| ARO-03 | `reports/aro-03-durable-long-task-runtime-report.md` |
| ARO-04 | `reports/aro-04-performance-cost-and-context-optimization-report.md` |
| ARO-05 | `reports/aro-05-release-regression-and-operating-model-report.md` |

## Dependency Flow

```text
ARO-00 Baseline Runtime and Industry Audit
  -> ARO-01 Middleware Harness and Approval Completion
  -> ARO-02 Trace Eval Feedback Loop
  -> ARO-03 Durable Long Task Runtime
  -> ARO-04 Performance Cost and Context Optimization
  -> ARO-05 Release Regression and Operating Model
```

## Validation Matrix

| PHASE_ID | Mutates Data | Needs Browser/UI | Needs Agent/LLM Eval | Needs Migration | Needs External Service | Release Blocking |
| --- | --- | --- | --- | --- | --- | --- |
| ARO-00 | no | no | design/evidence only | no | no | no |
| ARO-01 | no production data | no | middleware and approval eval evidence | no planned | no | yes |
| ARO-02 | local/dev eval rows only | yes if Eval UI changes | yes | unknown, prefer existing schema | no | yes |
| ARO-03 | local/dev checkpoint rows only | no | resume and trace evidence | yes, additive only | no | yes |
| ARO-04 | no production data | no | yes, golden/evaluator gate | unknown, prefer trace metadata | provider metadata fixtures only | yes |
| ARO-05 | no production data | yes if UI changed earlier | yes, whole-demand | no new migration | no | yes |

## Risk Matrix

| PHASE_ID | Primary Risk | Stop Condition |
| --- | --- | --- |
| ARO-00 | stale assessment or overclaiming tests | stop if repo facts or validation commands cannot be verified |
| ARO-01 | approval resume duplicates side effects or middleware bypasses deny | stop if idempotency or permission tests cannot pass |
| ARO-02 | trace datasets leak raw sensitive payloads or become SaaS-dependent | stop if redaction or tenant scoping cannot be proven |
| ARO-03 | checkpoint/resume stores unsafe data or migration is not additive | stop if duplicate tool execution cannot be prevented |
| ARO-04 | cost/latency optimization lowers answer quality | stop if golden/evaluator gates regress |
| ARO-05 | release claim misses cross-phase regression evidence | stop if any completed oracle item lacks actor and critic evidence |

## Runtime Artifacts

| Artifact | Path | Agent Rule |
| --- | --- | --- |
| Context Profile | `deploy/runbooks/assistant-runtime-optimization/context-profile.json` | Load first; it defines hot path, role budgets, and deferred triggers. |
| Loop State | `deploy/runbooks/assistant-runtime-optimization/loop-state.json` | Load first; keep active phase, feature, iteration, status, and next action current. |
| Loop Contract | `deploy/runbooks/assistant-runtime-optimization/loop-contract.json` | Deferred; open only when loop semantics are unclear. |
| Feature Oracle | `deploy/runbooks/assistant-runtime-optimization/feature-oracle.json` | Deferred; inspect only the selected feature item unless repairing oracle coverage. |
| Progress Log | `deploy/runbooks/assistant-runtime-optimization/progress-log.md` | Deferred; inspect recent entries only when blocker or status history is unclear. |
| Agent Handoff | `deploy/runbooks/assistant-runtime-optimization/agent-handoff.md` | Deferred; open only when next action or role handoff is unclear. |
| Continuity Ledger | `deploy/runbooks/assistant-runtime-optimization/continuity-ledger.md` | Deferred; open only dependency rows needed for target phase or writeback. |
| Next Window Prompt | `deploy/runbooks/assistant-runtime-optimization/next-window-prompt.md` | Deferred; open only when preparing a fresh context window. |

## Agent Role Handoffs

- Planner role: expand intent into phase contracts and feature-oracle cases without over-specifying implementation details.
- Generator role: execute one phase/feature item, update evidence, and hand off to evaluation.
- Critic role: run in an independent subagent or fresh context, review actor output from files and runtime checks, reject superficial completion, and write actionable findings.
- No phase or PRD completion claim is valid until independent critic evidence is recorded.

## Delivery Quality Gates

- Each phase is independently executable and verifiable.
- Each phase records inherited evidence, dependency status, and what it unlocks next.
- Each phase uses the smallest requirement-satisfying change; scope expansion must be justified in the report.
- Each phase records test evidence and independent critic evidence, or a blocker.
- The terminal phase or release gate runs whole-demand regression across completed feature-oracle items.
- Runtime files must be sufficient for a fresh agent to resume after context compaction.
- Cold start must use `context-profile.json`; do not load the full docs folder or every runtime file by default.
- `--strict` is structure readiness only; phase and full-demand completion require `--completion-gate`.

## Goal Setup Templates

Use the `goal.prompt` string from the phase's `## Machine Contract` JSON when creating an agent goal. If a phase has dependencies, do not execute it until dependency acceptance gates are met or explicitly waived in the previous phase report.

Example:

```text
Complete ARO-00 Baseline Runtime and Industry Audit for `.` by following `deploy/runbooks/assistant-runtime-optimization/phase-00-baseline-runtime-and-industry-audit.md`; stay inside named edit boundaries; finish only after code-summary writeback, continuity update, validation, regression, compliance, rollback, evidence, and acceptance gates pass or blockers are documented.
```

## Shared Agent Rules

- Use the exact phase `GOAL_PROMPT` when starting a goal.
- Open only `READ_FIRST` and `PRIMARY_CONTEXT` before planning.
- Make the smallest requirement-satisfying change.
- Expand edit scope only when a blocker is documented.
- Write test evidence, independent critic evidence, and the phase report before moving on.
- Run whole-demand regression in the terminal phase or release gate.

## External Inputs Checklist

- No external inputs are guaranteed by the scaffold.
- Record missing credentials, dashboards, Figma links, deployment access, migrations, and provider approvals before use.
