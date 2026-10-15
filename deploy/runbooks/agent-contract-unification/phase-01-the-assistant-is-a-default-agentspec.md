# Phase 01 - The assistant is a default AgentSpec

- PHASE_ID: ACU-01
- FEATURE_ID: ACU-F002
- DEPENDS_ON: ACU-00
- UNLOCKS: ACU-02, ACU-04

## Outcome

The built-in AI Assistant stops being a privileged code path and becomes the default `AgentSpec`
instance. One exported spec describes its identity, instructions, model policy, thinking policy,
tool posture, knowledge access, and permissions; an agent created from that exported spec behaves
the same as the built-in assistant on the same input.

This is the load-bearing phase of the program. Today `apps/assistant-service` never references
`AgentSpec` at all, so an agent built in Agent Studio is a strictly weaker thing than the assistant
we ship — and the gap widens with every assistant improvement. Law L1 in
`docs/harness/platform-architecture.md` exists to close it.

## Scope

In:
- A single source of truth for the default spec, versioned with the code that implements it.
- An endpoint or documented export that returns it (for example `GET /api/v1/agents/default-spec`).
- An equivalence test that runs the built-in assistant and a spec-built agent through the same
  input and compares observable behaviour.
- Extending `AgentSpec` wherever equivalence proves a field is missing — that finding is the point
  of this phase, not a reason to special-case the assistant.

Out:
- Changing how the assistant behaves. This phase describes current behaviour, it does not tune it.
- Migrating existing agents onto the default spec.
- Deleting the assistant's current code paths; the spec drives them, it does not replace them yet.

## Done when

- [ ] The default assistant spec is exported and is valid against the `AgentSpec` schema from ACU-00.
- [ ] An agent created from the exported spec advertises the same first-turn tool set as the built-in assistant for the same input.
- [ ] Both produce the same system prompt sections and the same effective thinking policy.
- [ ] Both resolve the same knowledge access for a session with no explicit dataset.
- [ ] Every assistant capability that the spec could not express is either added as a schema field or listed explicitly in `HANDOFF.md` as a remaining gap with a reason.
- [ ] `make test-isolation` passes.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Equivalence | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_default_agent_spec_equivalence.py` | A spec-built agent matches the built-in assistant on tools, prompt, and thinking policy |
| Schema validity | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_agent_spec_contract.py` | The exported default spec is valid AgentSpec, with no assistant-only escape hatch |
| Boundaries | `make test-isolation` | Service boundary and assistant OpenAPI contracts stay green |

## Stop or confirm

- Equivalence cannot be reached without changing assistant behaviour — that is a product decision about which behaviour is correct.
- Making the default spec editable by tenants; ownership of the default spec is an unresolved product decision recorded in `README.md`.
