# Agent Trace Eval PRD Harness Next Window Prompt

Use this prompt to start a fresh Codex, Claude Code, or Agent Skills-compatible window.

```text
Use $prd-phase-harness to continue the harness at `deploy/runbooks/agent-trace-eval-prd`.

Target phase: ATE-04
Target phase file: `deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md`
Target feature-oracle item: ATE-F005

Current status: verified terminal phase. ATE-00 through ATE-04 have passed. Future work should start as a new user-approved expansion for LangGraph Proxy Trace or RAG Trace.

Loading order:
1. Open `deploy/runbooks/agent-trace-eval-prd/context-profile.json`.
2. Open `deploy/runbooks/agent-trace-eval-prd/loop-state.json`.
3. Open only the target phase file: `deploy/runbooks/agent-trace-eval-prd/phase-04-release-regression-and-handoff.md`.
4. Do not open README, manifest, full source packet, full feature-oracle, progress log, handoff, continuity ledger, next-window prompt, or prior reports unless `context-profile.json` says the trigger applies.
5. Open only the target phase's hot-path `PRIMARY_CONTEXT` before planning.

Execution rule:
- Work on exactly one phase and one feature-oracle item when a new phase is approved.
- Follow the loop cycle: observe, select, execute, verify, record, decide.
- Plan before editing.
- Stay inside the phase edit boundaries and `LIKELY_EDIT_PATHS`.
- Run the required validation, regression, compliance, rollback, browser/runtime, and acceptance checks named by the phase.
- Summarize code facts and interface decisions back into targeted source packet and continuity ledger sections before handoff.
- Update the phase report, progress log, handoff file, continuity ledger, loop-state, and oracle evidence before claiming completion.
- Request an independent critic/subagent or fresh-context reviewer to write a separate critic artifact; actor self-review is not completion evidence.
- Preserve progressive disclosure: load additional files only when the context profile trigger is met.
- Stop and document blockers when credentials, production systems, destructive commands, production migrations, deployments, or out-of-scope edits are required.
- Treat strict validation as structure readiness; run strict completion gate only after the phase report and critic artifact exist.
```
