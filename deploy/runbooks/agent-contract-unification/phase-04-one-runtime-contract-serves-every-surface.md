# Phase 04 - One runtime contract serves every surface

- PHASE_ID: ACU-04
- FEATURE_ID: ACU-F005
- DEPENDS_ON: ACU-01
- UNLOCKS: ACU-05, ACU-06

## Outcome

There is one runtime contract, and our own console is one of its clients. The console assistant
page, the embed widget, and any future surface all reach an agent through the same endpoints and
consume the same event stream.

Today the console talks to `/api/v1/assistant` while the embed widget talks to
`/api/v1/agent-public/{public_id}` (`src/api/v1/agent_public.py`: config, chat/stream, feedback)
over the `agent-embed/v1` postMessage protocol. Two contracts means every new surface picks a side,
and neither side is authoritative. Law L2 in `docs/harness/platform-architecture.md` puts the
console on the public contract precisely because being our own first consumer is what keeps that
contract honest.

## Scope

In:
- Generalize the public runtime API so it can serve an authenticated console session as well as an
  anonymous embedded visitor. Authentication differs; the contract does not.
- Point the console assistant page at it.
- One documented event protocol for the stream, with the `agent-embed/v1` postMessage envelope
  defined in terms of it.
- Keep `AgentChannelPolicy` enforcement — `allowed_origins` and `requests_per_minute` — for public
  callers, and document what applies to authenticated callers.

Out:
- Removing `/api/v1/assistant`. Keep it working for existing clients for the whole program.
- Redesigning the console UI.
- Building new surfaces.

## Done when

- [ ] The console assistant page runs a full conversation through the public runtime endpoints.
- [ ] The embed widget and the console produce equivalent event streams for the same agent and input.
- [ ] Authenticated and anonymous callers are distinguished by credential, not by a separate endpoint shape.
- [ ] `allowed_origins` and `requests_per_minute` still reject a disallowed origin and an over-limit caller.
- [ ] The event protocol and the `agent-embed/v1` envelope are documented under `docs/`, not only in a runbook.
- [ ] `/api/v1/assistant` still serves existing clients unchanged.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| One contract | `uv run --all-packages --extra test pytest -q --no-cov tests/api/test_agent_public_runtime.py` | Authenticated and anonymous callers share the endpoint shape and event stream |
| Channel policy | `uv run --all-packages --extra test pytest -q --no-cov tests/security/test_agent_channel_policy.py` | A disallowed origin and an over-limit caller are still rejected |
| Console on the contract | `pnpm -C web e2e:opensource` | The console runs a full conversation through the public runtime endpoints |

## Stop or confirm

- Any change that lets an anonymous caller reach something only an authenticated console user could reach before.
- Enumerate exactly what became publicly reachable in this phase and report it, even when no confirmation is needed.
