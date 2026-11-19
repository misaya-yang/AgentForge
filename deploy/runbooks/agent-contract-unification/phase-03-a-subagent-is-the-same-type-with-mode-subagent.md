# Phase 03 - A subagent is the same type with mode=subagent

- PHASE_ID: ACU-03
- FEATURE_ID: ACU-F004
- DEPENDS_ON: ACU-00
- UNLOCKS: none

## Outcome

A subagent is an `AgentSpec` with `mode: subagent`, not a separate concept implemented in dispatch
code. The parent spawns it by referencing a spec; the child runs under its own permission ruleset
and budget; depth limits, approval fail-closed behaviour, and the no-parent-path rule are unchanged.

Subagent behaviour lives in `apps/assistant-service/src/assistant_service/core/agent/subagent_manager.py`
(1563 lines) plus `subagent_dispatch_runtime.py` and `subagent_types.py`. `opencode` models the same
thing as one field on a 38-line agent schema (`packages/schema/src/agent.ts`, `mode: "subagent" |
"primary" | "all"`). Unifying the type is what lets a builder define a domain subagent in Agent
Studio at all — today they cannot.

## Scope

In:
- Represent subagent definitions as `AgentSpec` values with `mode: subagent`.
- Resolve the child's permissions and budget from its own spec, bounded by the parent's — a child
  must never be able to widen what the parent may do.
- Keep `subagent_manager` as the executor; this phase changes what it is handed, not how it runs.
- Preserve existing depth limits, approval gateway fail-closed behaviour, and output contracts.

Out:
- Exposing subagent authoring in the Agent Studio UI. That is a later product phase.
- Rewriting `subagent_manager` internals.
- Changing the parent-child streaming or output contract.

## Done when

- [ ] A subagent defined as an `AgentSpec` with `mode: subagent` can be spawned from a parent run.
- [ ] The child's deny rules are enforced, and a rule the parent denies stays denied for the child.
- [ ] The child's step budget is enforced independently of the parent's.
- [ ] An `AgentSpec` with `mode: primary` is rejected as a subagent target with a clear error.
- [ ] Existing depth limits, approval fail-closed behaviour, and `test_parent_harness_freedom.py` expectations are unchanged.
- [ ] `make verify-assistant-runtime-dev` passes.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Spec-driven subagent | `uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant/test_subagent_manager.py` | A `mode: subagent` spec spawns, and its own ruleset and budget are enforced |
| Parent boundary | `uv run --all-packages --extra test pytest -q --no-cov tests/services/assistant/test_parent_harness_freedom.py` | A child cannot widen what the parent is allowed to do |
| Runtime gate | `make verify-assistant-runtime-dev` | Depth limits, approval fail-closed behaviour, and output contracts are unchanged |

## Stop or confirm

- Any change to approval semantics or depth-limit behaviour.
- A design that appears to require a child widening parent permissions — report it as a security finding rather than implementing it.
