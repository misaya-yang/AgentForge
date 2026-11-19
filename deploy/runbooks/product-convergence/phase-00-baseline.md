# PC-00 — Baseline

Feature: PC-00-F1. The working tree's pre-existing uncommitted changes are checkpointed on a
dedicated branch, and the harness contract is green before any product work starts.

## Steps

1. `git checkout -b product-convergence/main` (from main with dirty tree).
2. Commit the pre-existing changes as checkpoint `chore: checkpoint pre-convergence working tree`.
3. Write the runbook skeleton (README, loop-state.json, feature-oracle.json, progress-log, handoff).
4. Gate: `make harness-check`.

## Evidence

- [ ] `git log --oneline -1` = `bf74ff6 chore: checkpoint pre-convergence working tree`
- [ ] `make harness-check` exit 0 (command output quoted in loop-state)
