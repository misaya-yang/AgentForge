# AS-00 Execution Plan

- Phase: `AS-00`
- Feature Oracle: `AS-F001`
- Baseline: clean `main` at `945eb2225d644093802bf5f9d75ca4d9dbad6a8d`, equal to the locally recorded `origin/main` at the same commit
- Boundary: the existing Assistant runtime only; no Agent schema, CRUD, UI, deployment, Docker, migration, or public API changes

## Requirement and gate map

| Requirement / gate | Intended files | Verification | Review and evidence |
| --- | --- | --- | --- |
| R1 branch-accurate baseline | this plan, AS-00 report, targeted source-packet facts | `branch-baseline` command from the phase contract | Record exact commit/ref relation and the empty `HEAD..origin/main` path diff; do not sync or switch branches. |
| R2 honest capability inventory | AS-00 report plus targeted source-packet and continuity-ledger rows | Static trace from Assistant/Gateway composition roots and management routes; integration assertions for capability families | Distinguish platform-native tools, model-native search, MCP, Skills, Connectors, and Knowledge; record setup/health and proven reachability rather than inferring from comments. |
| R3 non-expanding allowlist | `agent_loop.py`, `tool_invoker.py`, `test_agent_capability_allowlist.py`, `test_assistant_capability_wiring.py` | Required capability pytest command | Add one typed request-context allowlist where `None` preserves the legacy set and any explicit set, including empty, filters before relevance selection and is rechecked before invocation. Apply the same bound after connector visibility merging so connectors cannot expand it. |
| R4 compatibility and handoff | tests and AS-00 durable evidence files | `make test-isolation`, targeted Ruff, phase completion gate | Preserve existing public Assistant inputs and defaults; assign MCP/Skill/Connector wiring gaps only to their existing downstream phases. |

## Minimal-change and rollback method

The implementation seam will remain internal to `AgentLoopConfig` / `ToolInvocationContext` and `RegistryToolInvoker`. Existing callers omit it and therefore retain current behavior. The actor will inspect only the AS-00 diff, run the four required checks, and record any broader test failures without converting them to passes. Rollback is the isolated allowlist seam and its tests; read-only baseline/capability evidence remains useful.

## Durable writeback targets

- `source-packet.md`: correct the branch baseline and record verified composition-root/route facts.
- `continuity-ledger.md`: record the allowlist type, `None` compatibility contract, explicit-empty semantics, connector post-merge bound, and invocation recheck.
- AS-00 report / `feature-oracle.json`: cite actual command results and changed files only.
- `progress-log.md`, `agent-handoff.md`, `loop-state.json`, and `next-window-prompt.md`: update only after actor validation and independent Critic evidence determine whether AS-01 unlocks.
