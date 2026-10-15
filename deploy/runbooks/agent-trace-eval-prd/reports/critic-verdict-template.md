# Critic Verdict Template

**Phase:** Record concrete phase id and name.

**Feature:** Record concrete feature-oracle id.

**Critic:** Record independent-subagent, fresh-context critic, or separate reviewer identity.

**Critic Verdict:** Use approved, changes_requested, blocked, or waived.

**Actor Report Reviewed:** Record the actor phase report path.

**Date:** Record the review date.

---

## Critic Inputs

- Phase contract: record path.
- Feature oracle item: record id and status.
- Actor report: record path.
- Changed files or diff: record command or file list.
- Validation evidence: record command output summary.
- Runtime/browser/eval evidence: record evidence path.
- Minimal-change boundary: record assessment.
- Regression scope: record assessment.

## Findings

State concrete acceptance or rejection findings. Do not approve based on actor self-review alone.

## Requirement Coverage

Map actor evidence to phase acceptance gates and feature-oracle expectations.

## Test and Regression Assessment

Confirm which commands and checks were inspected, whether they passed, and what remains blocked.

## Minimal-Change Assessment

Confirm changed files stayed inside the phase boundary or explain justified scope expansion.

## Whole-Demand Regression Assessment

Required for terminal phase or full-demand completion. State whether whole-demand regression evidence was present and sufficient.

## Waiver Reason

Required only when `Critic Verdict: waived`.
