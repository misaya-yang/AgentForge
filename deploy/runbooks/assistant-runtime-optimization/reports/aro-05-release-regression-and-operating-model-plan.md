# ARO-05 Release Regression and Operating Model Plan

**Phase:** ARO-05 Release Regression and Operating Model

**Feature:** ARO-F006

**Status:** planned

**Date:** 2026-06-29

## Assumption

ARO-05 is a terminal release-evidence phase. It should not deploy, run production migrations, or change application source unless a release-blocking regression is proven by the required gates.

## Plan

1. Run the required regression gates from the phase contract:
   - `make verify-eval-dev`
   - `make validate-example-config`
2. Create an operating model/runbook under the harness folder documenting:
   - phase decisions and rollback boundaries;
   - trusted verification commands;
   - SLO/no-go thresholds;
   - feature flags or disabled future rollouts;
   - no-deploy release boundary.
3. Update ARO-F006 evidence and whole-demand status across the feature oracle, source packet, continuity ledger, progress log, handoff, and next-window prompt.
4. Write the ARO-05 actor report and independent critic artifact.
5. Run both completion gates:
   - ARO-05 phase gate;
   - full harness completion gate.

## Likely Files

- `deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-report.md`
- `deploy/runbooks/assistant-runtime-optimization/reports/aro-05-release-regression-and-operating-model-critic.md`
- `deploy/runbooks/assistant-runtime-optimization/operating-model.md`
- `deploy/runbooks/assistant-runtime-optimization/feature-oracle.json`
- `deploy/runbooks/assistant-runtime-optimization/progress-log.md`
- `deploy/runbooks/assistant-runtime-optimization/continuity-ledger.md`
- `deploy/runbooks/assistant-runtime-optimization/source-packet.md`
- `deploy/runbooks/assistant-runtime-optimization/agent-handoff.md`
- `deploy/runbooks/assistant-runtime-optimization/next-window-prompt.md`
- `deploy/runbooks/assistant-runtime-optimization/loop-state.json`

## Validation

| Gate | Command |
| --- | --- |
| eval-dev-bundle | `make verify-eval-dev` |
| open-source-config | `make validate-example-config` |
| ARO-05 completion | `python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --phase ARO-05 --quality-score` |
| Full completion | `python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --quality-score` |

## Minimal-Change Boundary

Do not edit application source unless a required release gate fails due to an ARO change and the fix is smaller than documenting a blocker. Do not deploy or mutate production data.
