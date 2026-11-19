# PHASE-ID Actor Report Template

Copy this file to the report path named by the target Phase. Replace every bracketed instruction with executed evidence; never mark `passed` while an instruction remains.

**Phase:** [exact Phase ID and title]  
**Status:** [passed, blocked, or waived]  
**Date:** [ISO date]  
**Actor:** [agent or reviewer identity]

## Summary

[State the observable outcome, not just files edited.]

## Plan Followed

- Plan artifact: [path]
- Deviations: [none, or exact deviation and reason]

## Files Changed and Minimal Scope

| File | Why required | Why this is the smallest sufficient boundary |
| --- | --- | --- |
| [path] | [requirement] | [scope evidence] |

## Requirement Results

| Requirement | Result | Evidence |
| --- | --- | --- |
| [R-ID] | [passed/blocked/waived] | [command, browser, trace, schema or report path] |

## Validation Evidence

| Gate ID | Exact command/check | Exit/result | Durable output |
| --- | --- | --- | --- |
| [gate] | [exact command or browser procedure] | [passed/failed/skipped] | [path or concise result] |

Do not write `passed` for a command that was not executed. A skipped check must name the blocker, evidence collected, residual risk and whether downstream work remains locked.

## Browser, Runtime, Eval, Security, and Migration Evidence

- Browser matrix/screenshots: [paths or not applicable with Phase reason]
- Runtime/Trace/golden table: [paths or not applicable with Phase reason]
- Security/compliance matrix: [path]
- Migration/idempotency/rollback: [path or not applicable with Phase reason]
- Console/network summary: [path or not applicable with Phase reason]

## Feature Oracle Update

| Feature | Old status | New status | Actor evidence |
| --- | --- | --- | --- |
| [AS-Fxxx] | failing/blocked | passing/blocked/waived | [this report plus deterministic evidence] |

`passing` is not valid until the separate Critic artifact approves this report.

## Independent Critic

- Requested critic scope: Phase contract, Oracle item, diff, validation logs, regression, rollback and minimal-change boundary.
- Critic artifact: [reports/as-xx-critic-verdict.md]
- Verdict: [approved, changes_requested, blocked, or waived]
- Findings resolved: [IDs and evidence]

## Compliance, Rollback, and Residual Risk

- Compliance gates: [per-gate result]
- Rollback tested: [exact result]
- Blockers/waivers: [none or exact user-approved waiver]
- Residual risks: [owner, trigger and next action]

## Runtime Artifact Writeback

- `feature-oracle.json`: [fields changed]
- `loop-state.json`: [decision/status changed]
- `progress-log.md`: [entry]
- `agent-handoff.md`: [next role/action]
- `continuity-ledger.md`: [interface decisions]
- `source-packet.md`: [new or corrected code facts]

## Handoff

[State whether the next Phase is unlocked. If blocked, name the exact missing evidence and stop condition.]
