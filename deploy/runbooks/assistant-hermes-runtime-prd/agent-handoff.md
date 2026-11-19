# Assistant Hermes OpenClaw Runtime PRD Agent Handoff

**Created:** 2026-06-30

**Harness Folder:** `deploy/runbooks/assistant-hermes-runtime-prd`

---

## Planner Notes

- Product PRD: `deploy/runbooks/assistant-hermes-runtime-prd/product-prd.md`
- Context profile: `deploy/runbooks/assistant-hermes-runtime-prd/context-profile.json`
- Source packet: `deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md`
- OpenClaw synthesis: `deploy/runbooks/assistant-hermes-runtime-prd/openclaw-synthesis.md`
- Feature oracle: `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json`
- Continuity ledger: `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md`
- First target phase: `AHR-00`
- First target phase file: `deploy/runbooks/assistant-hermes-runtime-prd/phase-00-comparative-baseline-evidence.md`
- First feature-oracle item: `AHR-F001`
- Core decision: keep AI--Platfform's enterprise runtime/eval foundation; absorb Hermes patterns for runtime completeness, memory lifecycle, observer hooks, and regression discipline; absorb OpenClaw patterns for context compiler, layered memory, skills/plugins separation, prompt hook governance, canonical approval plans, and read-only doctor/status.

## Comparison Notes

- Runtime: Hermes has broader entrypoints, provider transports, turn diagnostics, and long-task recovery. AI--Platfform should add a durable run/session/turn envelope instead of creating a new shell.
- Memory: Hermes has clearer MEMORY.md/USER.md semantics, completed-turn sync, provider lifecycle, and compaction lineage. AI--Platfform should implement equivalent lifecycle discipline on its DB-backed memory model.
- Tool safety: AI--Platfform's ExecutionGateway, safe_fetch, and Docker/gVisor posture are stronger, but any direct risky registry path must fail closed.
- Eval/observability: AI--Platfform already leads with dataset/evaluator/experiment/run/gate and `/eval`; Hermes trajectory/observer ideas should enrich capture, not replace the platform.
- Security: Hermes' honest local-execution threat model should become AI--Platfform PRD gates: approvals and allowlists are not containment; sandbox and gateway boundaries are required.
- OpenClaw runtime discipline: system prompt/context must be compiled from real runtime state, not static capability claims. Record context snapshots and fail tests on prompt/runtime capability drift.
- OpenClaw memory discipline: harness files, durable memory, session transcript, trace, checkpoint, retrieval index, and pre-compaction flush are separate layers.
- OpenClaw governance discipline: skills are knowledge packages; plugins/connectors/MCP/internal tools are executable capability requiring trust, risk, approval, audit, redaction, and prompt-exposure metadata.
- OpenClaw ops discipline: doctor/status should be read-only, redacted, bounded, and offline-first before any repair or migration behavior is considered.

## Generator Notes

- Work on one phase and one feature-oracle item at a time.
- Load `context-profile.json`, `loop-state.json`, and the target phase first; defer broad runtime files until a trigger applies.
- Stay inside the phase `LIKELY_EDIT_PATHS`.
- Make the smallest requirement-satisfying change and justify any scope expansion.
- Summarize inspected code facts into targeted sections of `deploy/runbooks/assistant-hermes-runtime-prd/source-packet.md`.
- Update `deploy/runbooks/assistant-hermes-runtime-prd/progress-log.md`, `deploy/runbooks/assistant-hermes-runtime-prd/continuity-ledger.md`, and the phase report before handoff.
- Record test evidence and independent critic evidence before marking a phase passed.
- Run `--strict --completion-gate --phase <PHASE_ID>` before marking a phase passed or unlocked.

## Critic Notes

- Read the phase report, changed files, validation output, and oracle evidence.
- Reject `passing` status when evidence is missing, superficial, outside the target phase, missing independent critic approval, broader than the minimal required change, or cites a blocked/partial report.
- Confirm terminal whole-demand regression and full `--completion-gate` before the full requirement is considered complete.
- Write findings as actionable file/line or command/check notes.

## Next Handoff

- Active role: complete
- Active phase: AHR-05 (terminal, passed)
- Active feature-oracle item: AHR-F006 (passing)
- Terminal status: All six feature-oracle items (AHR-F001 through AHR-F006) are passing. The full requirement chain AHR-00 through AHR-05 is complete.
- Required evidence before terminal closure: **complete** — AHR-05 actor phase report, operating model runbook, offline release-gate output, whole-demand regression across AHR-F001 through AHR-F006, oracle evidence, independent critic verdict, minimal-change scope note, progress-log entry, continuity-ledger update, and code-summary writeback are all recorded.
- AHR-00 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-00-comparative-baseline-evidence-critic.md`.
- AHR-01 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-01-entry-session-and-turn-contract-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-01-entry-session-and-turn-contract-critic.md`.
- AHR-02 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-02-memory-context-and-compaction-lineage-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-02-memory-context-and-compaction-lineage-critic.md`.
- AHR-03 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-03-tool-permission-and-runtime-safety-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-03-tool-permission-and-runtime-safety-critic.md`.
- AHR-04 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-04-observability-eval-and-regression-cockpit-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-04-observability-eval-and-regression-cockpit-critic.md`.
- AHR-05 status: passed. Evidence lives in `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-05-operating-model-and-release-gate-report.md` and `deploy/runbooks/assistant-hermes-runtime-prd/reports/ahr-05-operating-model-and-release-gate-critic.md`.
- Next concrete action: No further phases. Future work should start from a new PRD or extension phase. Promote `make verify-assistant-runtime-dev` to optional/manual CI stage after 3+ stable consecutive runs.
