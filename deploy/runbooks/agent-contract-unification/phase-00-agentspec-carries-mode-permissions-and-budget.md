# Phase 00 - AgentSpec carries mode, permissions, and budget

- PHASE_ID: ACU-00
- FEATURE_ID: ACU-F001
- DEPENDS_ON: none
- UNLOCKS: ACU-01, ACU-03

## Outcome

`AgentSpec` can declare three things it cannot declare today: which role the agent plays
(`mode`), what it is allowed to touch (`permissions`), and how much it may spend (`budget`).
Existing agents keep loading and behaving exactly as before, and every existing `capabilities`
binding keeps its meaning.

Today `AgentSpec` (`src/api/schemas/agents.py:68`) holds only `identity`, `instructions`, `model`,
`capabilities`, `knowledge`, and `memory`. `memory` is an untyped `dict[str, Any]`, which is a
placeholder rather than a design. Capability bindings are static (`resource_id` +
`resource_version` + `schema_hash`), so an agent never inherits a tool the platform adds later —
that is what `permissions` fixes, without removing the pinning that `capabilities` provides.

## Scope

In:
- `src/api/schemas/agents.py` — add `mode`, `permissions`, `budget` to `AgentSpec`.
- A permission ruleset type: ordered allow/deny rules over tool-name and resource patterns, with a
  documented evaluation order and a default-deny or default-allow decision recorded in the file.
- Resolution: where a resolved spec is produced, carry the three new fields through.
- `tests/api/test_agents_api.py` and a new schema test for defaults, round-trip, and rule evaluation.
- `docs/harness/platform-architecture.md` §3 stays the reference for why both mechanisms exist.

Out:
- Enforcing the ruleset inside the agent loop. This phase only makes it declarable and resolvable.
- Removing, weakening, or migrating any `capabilities` binding.
- Typing `memory`. Record it as a known gap in `HANDOFF.md` instead.
- Any change to `apps/assistant-service`.
- Database migration, unless persistence genuinely cannot hold the new fields; if it can only be
  done with a migration, stop and confirm first.

## Done when

- [ ] An `AgentSpec` JSON with none of the three new fields deserializes, resolves, and produces the same runtime behaviour as before.
- [ ] `mode` accepts `primary`, `subagent`, and `all`, and defaults to `primary`.
- [ ] A deny rule matching a tool name causes the resolver to report that tool as denied.
- [ ] An allow rule lets an agent reach a tool that is not named in its `capabilities` list.
- [ ] A `capabilities` binding with a pinned `resource_version` still resolves to that exact version.
- [ ] `budget` carries at least a step limit, and an absent budget means the current default.
- [ ] `tests/api/test_agents_api.py` passes unchanged.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Backward compatible | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_agents_api.py` | Existing agents load and resolve unchanged |
| New fields | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_agent_spec_contract.py` | Defaults, round-trip, rule evaluation order, and a pinned capability version still honoured |
| Lint | `uv run --all-packages --extra dev ruff check src/api/schemas/agents.py src/api/v1/agents.py` | Touched code is clean |

## Stop or confirm

- Adding a database migration or changing the persisted shape of stored agent versions.
- Choosing default-deny for the ruleset if it would change any existing agent's behaviour.
- Report which default the ruleset uses and the reason, even when no confirmation is needed.
