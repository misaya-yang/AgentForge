# Agent Contract Unification

- Owner: AI--Platfform
- Repository: `/Users/yang/projects/AI--Platfform`
- Target architecture: [`docs/harness/platform-architecture.md`](../../../docs/harness/platform-architecture.md)

## Goal

Make the built-in AI Assistant an instance of AgentSpec and make the public runtime API the only contract every surface uses, so a new product form or capability can be added without changing the agent kernel.

## Non-goals

- Rewriting the agent loop, approval gateway, memory lifecycle, or trace pipeline.
- Building a second agent-framework adapter. ACU-06 only defines the seam.
- Shipping new surfaces (Feishu, ACP, desktop). This program makes them cheap; it does not build them.
- Replacing `capabilities` with `permissions`. Both stay — see `platform-architecture.md` §3.
- Removing the legacy `ChatRequest` fields. ACU-02 makes the spec authoritative and deprecates
  them; deletion is a later decision.

## Authorization

- Safe local reads, in-scope edits, and non-destructive tests may proceed without asking.
- Confirm before: applying a migration to a live database, Docker actions, deploys, commits, pushes,
  and any destructive or irreversible operation.
- Never print secrets. Follow [`docs/harness/runtime-and-secrets.md`](../../../docs/harness/runtime-and-secrets.md)
  before any Docker, deploy, or E2E action.
- Do not commit unless the user asks.

## Phase Map

| Phase | Feature | Contract | Depends on |
| --- | --- | --- | --- |
| ACU-00 | ACU-F001 | [AgentSpec carries mode, permissions, and budget](phase-00-agentspec-carries-mode-permissions-and-budget.md) | none |
| ACU-01 | ACU-F002 | [The assistant is a default AgentSpec](phase-01-the-assistant-is-a-default-agentspec.md) | ACU-00 |
| ACU-02 | ACU-F003 | [Builder settings live in the spec, not the request](phase-02-builder-settings-live-in-the-spec-not-the-request.md) | ACU-01 |
| ACU-03 | ACU-F004 | [A subagent is the same type with mode=subagent](phase-03-a-subagent-is-the-same-type-with-mode-subagent.md) | ACU-00 |
| ACU-04 | ACU-F005 | [One runtime contract serves every surface](phase-04-one-runtime-contract-serves-every-surface.md) | ACU-01 |
| ACU-05 | ACU-F006 | [SDKs speak the public runtime contract](phase-05-sdks-speak-the-public-runtime-contract.md) | ACU-04 |
| ACU-06 | ACU-F007 | [Adding a surface or capability is documented and gated](phase-06-adding-a-surface-or-capability-is-documented-and-gated.md) | ACU-04 |

`loop-state.json` is authoritative for status.

## Operating Rules

1. Cold start: read `loop-state.json`, the active feature, `HANDOFF.md`, `init.sh`, and the active phase file.
2. Run `./init.sh`, then the active phase's `## Verify` command, before changing anything.
3. One feature per `observe → act → verify → decide` cycle. Do not batch phases.
4. Set `passes: true` only with evidence: a named command and its real output.
5. Every change is additive first. Existing agents, existing `ChatRequest` payloads, and existing
   `/api/v1/assistant` clients must keep working through the whole program.
6. `permissions` and `capabilities` coexist. Do not delete `capabilities` bindings anywhere.
7. If a phase reveals that the assistant needs something `AgentSpec` cannot express, that is the
   finding — extend the schema, do not special-case the assistant.
