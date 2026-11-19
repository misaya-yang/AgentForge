# PHASE-ID Independent Critic Verdict Template

Copy this file to `reports/as-xx-critic-verdict.md` after the Actor report exists. The Critic must be an independent subagent, fresh context, or separate reviewer; Actor self-review is invalid.

**Phase:** [exact Phase ID and title]  
**Feature:** [exact Oracle ID]  
**Critic:** [independent reviewer identity]  
**Critic Verdict:** [approved, changes_requested, blocked, or waived]  
**Actor Report:** [path]  
**Date:** [ISO date]

## Inputs Reviewed

- Phase contract: [path]
- Oracle item: [ID]
- Actor report: [path]
- Diff/changed files: [command or path]
- Validation evidence: [paths]
- Browser/runtime/eval evidence: [paths]
- Security/migration/rollback evidence: [paths]

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| [C-01] | [critical/high/medium/low] | [R-ID/gate] | [specific evidence-backed finding] | [specific correction or none] |

## Requirement Coverage

[Map each Phase requirement and Oracle step to Actor evidence; identify missing or superficial coverage.]

## Test and Regression Assessment

[Confirm exact commands/checks reviewed, whether output supports the claim, and whether the regression scope is sufficient.]

## Security, Privacy, and Failure Assessment

[Assess tenant/ACL, Secret, Prompt/tool boundaries, fail-closed behavior, data retention and external-service behavior triggered by this Phase.]

## Minimal-Change Assessment

[Confirm the diff stayed within `likely_edit_paths`, explain justified expansion, and reject unrelated refactors or formatting.]

## Rollback and Handoff Assessment

[Confirm rollback evidence, continuity writeback, downstream contract and unlock decision.]

## Whole-Demand Regression Assessment

[For AS-09, evaluate all Oracle items, same-build aggregate integrity and terminal rerun evidence. For earlier phases, state that terminal whole-demand regression remains pending and assess only inherited regression.]

## Verdict Rationale

[Explain why the evidence supports approved/changes_requested/blocked. A waived verdict must quote the user's explicit waiver scope and residual risk.]
