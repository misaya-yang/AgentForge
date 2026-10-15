# Phase 01 - Runtime, app-server, Capability Worker, Gateway, and web compatibility adaptations form one source-green integration.

- PHASE_ID: ARU-01
- FEATURE_ID: ARU-F002
- DEPENDS_ON: ARU-00

## Outcome

All platform extension seams compile against the selected upstream API without changing Gateway, ThreadStore, Capability Worker, approval, Agent configuration/Eval, or public API ownership.

## Scope

In:

- Runtime core/app-server/platform crates under `rust/agent-runtime-overlay/kernel-rs/`.
- Gateway, web, Agent configuration, and Agent Eval adapters only when an actual platform contract changed.

Out:

- Database redesign, upstream-only product surfaces, new services, or whole-repository refactors.

## Done when

- [x] Cross-module APIs and platform extension seams are internally consistent with no public contract drift.
- [x] Agent preview, versioned configuration, candidate trace persistence, evaluation, publish, and rollback retain their existing platform ownership.
- [x] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Static repository contract | `make harness-check` | Existing repository and placement contracts pass. |
| Public contracts | `make agent-runtime-single-kernel-gate && make sdk-sse-contract` | Gateway OpenAPI/SSE/SDK behavior remains compatible. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Rust fmt/check/test evidence comes from hosted CI or Docker-contained commands only; never invoke host Cargo/Rust tools.
