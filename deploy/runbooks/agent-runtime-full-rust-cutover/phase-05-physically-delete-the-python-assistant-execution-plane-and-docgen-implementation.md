# Phase 05 - Physically delete the Python Assistant execution plane and docgen implementation

- PHASE_ID: FRC-05
- FEATURE_ID: FRC-F006
- DEPENDS_ON: FRC-04

## Outcome

The source tree, workspace, Compose distribution, tests, and documentation contain no Python Assistant execution plane or Python docgen implementation.

## Scope

In:

- Delete replaced Python packages, legacy routes/proxies, dependencies, images, environment values, tests and Eval imports; add permanent AST/import/Compose/source-distribution guards.

Out:

- Deleting Gateway, Knowledge, Local Node, Web, plugin/skill data assets, migration evidence, or persistent schemas.
- Weakening or deleting compatibility fixtures merely because their Python implementation is gone.

## Done when

- [ ] `apps/assistant-service`, Python docgen, AgentLoop/SubAgentManager/DAG/Swarm/streaming tool loop, unreachable handler bodies and `_assistant_proxy` are absent.
- [ ] No Assistant Service container/image/URL/port/workspace dependency remains.
- [ ] Every deleted behavior test maps to a Rust, contract, Gateway, or real-product replacement test.
- [ ] Architecture gates reject any future Python model/tool loop or bypass of Runtime.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Physical absence | `make agent-runtime-no-python-loop-gate` | Deleted modules, imports, dependencies, routes, container and internal URL cannot reappear. |
| Contracts | `make agent-runtime-single-kernel-gate && make harness-check` | Rust remains the only kernel and repository contracts are coherent. |
| Distribution | `make validate-config` | New users receive Runtime plus Worker without Assistant Service. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Independent review must approve the deletion mapping and confirm no behavior was removed without passing replacement evidence.
