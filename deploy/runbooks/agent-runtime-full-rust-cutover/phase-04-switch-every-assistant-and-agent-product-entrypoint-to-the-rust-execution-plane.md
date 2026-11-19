# Phase 04 - Switch every Assistant and Agent product entrypoint to the Rust execution plane

- PHASE_ID: FRC-04
- FEATURE_ID: FRC-F005
- DEPENDS_ON: FRC-03

## Outcome

Every public Assistant and Agent product entrypoint runs through the Rust execution plane with unchanged contracts and complete UI behavior.

## Scope

In:

- Assistant, Responses, Studio Preview/Eval/Publish, Hosted/Embed/Public Agent, Knowledge, Office, Artifacts, Quiz, images, MCP/Connector, Local Node, SDK, CLI, public OpenAPI and SSE projections.

Out:

- Physical Python deletion before all entrypoint evidence passes.
- Production rollout.

## Done when

- [ ] OpenAPI and SDK compatibility gates show no unapproved public drift.
- [ ] Authenticated desktop/mobile, light/dark, long-stream, thinking, Activity, approval, Artifact and download journeys pass for every product surface.
- [ ] Real Qwen and every enabled provider complete simple, complex, tool, multi-Agent, stop/follow-up and long-context scenarios with zero Provider 400.
- [ ] Assistant Service model traffic remains zero for the complete matrix.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Product cutover | `make verify-agent-product-runtime-cutover` | Every model-producing public function enters Runtime. |
| Web/SDK | `make sdk-sse-contract && pnpm -C web type-check && pnpm -C web lint && pnpm -C web build` | Public types, streams, and bundles remain valid. |
| Browser/live | Full authenticated browser and provider matrix | Every named product surface works without Python execution. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Any missing product route, provider error, UI interruption, unpaired tool call, or Python model traffic blocks deletion.
