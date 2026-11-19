# Assistant Hermes OpenClaw Runtime PRD Next Window Prompt

The requirement chain AHR-00 through AHR-05 is **complete**. All six feature-oracle items (AHR-F001 through AHR-F006) are passing.

Use this prompt to start a fresh Codex, Claude Code, or Agent Skills-compatible window for post-completion review or extension work.

```text
The harness at `deploy/runbooks/assistant-hermes-runtime-prd` is complete.

All phases passed: AHR-00, AHR-01, AHR-02, AHR-03, AHR-04, AHR-05.
All feature-oracle items are passing: AHR-F001 through AHR-F006.

For post-completion review or extension:
1. Open `deploy/runbooks/assistant-hermes-runtime-prd/loop-state.json` to confirm terminal status.
2. Open `deploy/runbooks/assistant-hermes-runtime-prd/feature-oracle.json` to verify all items.
3. Open `deploy/runbooks/assistant-runtime-operating-model.md` for the operating model.
4. Run `make verify-assistant-runtime-dev` to verify the offline regression gate.
5. Run `make verify-eval-dev && make eval-regression-gate` for the eval baseline.

Future work should start from a new PRD or extension phase.
```
