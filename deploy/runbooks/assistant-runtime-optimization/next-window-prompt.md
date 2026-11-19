# Next Window Prompt

Use the `prd-phase-harness` skill.

Repo: `the project root`

Harness: `deploy/runbooks/assistant-runtime-optimization`

Status: all phases `ARO-00` through `ARO-05` are complete after the full completion gate passes.

Target phase: `ARO-05`

Target phase file: `deploy/runbooks/assistant-runtime-optimization/phase-05-release-regression-and-operating-model.md`

Target feature: `ARO-F006`

Loading order for audit or repair:

1. `deploy/runbooks/assistant-runtime-optimization/context-profile.json`
2. `deploy/runbooks/assistant-runtime-optimization/loop-state.json`
3. `deploy/runbooks/assistant-runtime-optimization/feature-oracle.json`
4. The specific report or phase file requested by the user.

Progressive disclosure rule: do not load the full harness folder, full source packet, old reports, or unrelated source files unless the user asks for audit/repair and the target file says the trigger applies.

Loop cycle for audit or repair: observe, select, execute, verify, record, decide.

One phase and feature rule: execute only one phase and feature per repair pass. The completed target phase is `ARO-05` and the completed target feature is `ARO-F006`.

Edit boundaries: do not re-run implementation phases unless the user explicitly asks for repair or new scope. For deployment, dashboarding, or adaptive-routing experiments, start a new goal with a new plan; this harness intentionally did not deploy or mutate production.

Evidence and continuity ledger rule: any repair must update the actor report, critic artifact, feature oracle evidence, progress log, source packet code summary writeback, continuity ledger, handoff, and next-window prompt before rerunning gates.

Final validation commands:

```bash
python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --phase ARO-05 --quality-score
python3 validate_harness_prd.py deploy/runbooks/assistant-runtime-optimization --strict --completion-gate --quality-score
```

If either command fails, repair the named artifact and rerun both gates before claiming completion.

Stop conditions: stop and record a blocker if production deploy is requested without explicit approval, credentials are required, destructive commands are needed, feature oracle evidence is inconsistent, or validation cannot run locally.
