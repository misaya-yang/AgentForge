# Phase 02 - Migrate read-only capabilities and Gateway control ownership

- PHASE_ID: FRC-02
- FEATURE_ID: FRC-F003
- DEPENDS_ON: FRC-01

## Outcome

All original read-only capabilities and control APIs work through Gateway, Rust Runtime, and Rust Worker with catalog and result parity.

## Scope

In:

- Knowledge, web, file search/read, Artifact read, memory load, attachments, citations/evidence, MCP/Connector discovery, tool search/describe, and Gateway-owned models/config/datasets/sessions/history.

Out:

- State-changing tools, Office generation, image generation, Local Node actions, or Python deletion.

## Done when

- [ ] Rust catalog matches every frozen read descriptor name, schema hash, risk, permission, visibility, and alias.
- [ ] Real Knowledge, attachments, memory, web, Artifact and MCP read journeys return grounded evidence with tenant isolation.
- [ ] Gateway control APIs no longer proxy these responsibilities to Assistant Service.
- [ ] Instrumentation records zero Python read-path calls during the full matrix.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Catalog parity | `make agent-capability-read-parity-gate` | Frozen Python and Rust read contracts are exactly compatible. |
| Runtime integration | `make agent-runtime-single-kernel-gate` | V1/V2 public flows use only the Runtime and Worker. |
| Live reads | Authenticated Knowledge/attachment/memory/MCP browser matrix | User-visible read capabilities work on the real stack. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Any catalog mismatch, cross-tenant observation, missing citation/evidence, or Python read call blocks FRC-03.
