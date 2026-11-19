# Phase 01 - Implement the Rust capability execution contract and worker

- PHASE_ID: FRC-01
- FEATURE_ID: FRC-F002
- DEPENDS_ON: FRC-00

## Outcome

One Rust Runtime turn can discover, dispatch, resume, cancel, and terminalize a Rust Worker capability through Capability Contract V2 without Python Assistant code.

## Scope

In:

- Rust contract, worker, and Office crate skeletons; signed leases; catalog/execution/event/cancel HTTP APIs; additive execution/event tables; Runtime client; two locked OCI artifacts.

Out:

- Product-specific tool ports beyond a deterministic read-only fixture.
- Public Gateway API changes or Python deletion.

## Done when

- [ ] Strict descriptor, lease, execution, event, effect, approval, and terminal schemas are shared by Runtime and Worker.
- [ ] `(run_id, tool_call_id, attempt_id)` and idempotency conflicts are enforced atomically.
- [ ] Disconnect, duplicate dispatch, worker restart, scope forgery, expiration, cancel, and event-cursor resume tests pass.
- [ ] Runtime producer-ahead is bounded and every published fixture call gets exactly one terminal result.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Rust contract | `CARGO_BUILD_JOBS=1 cargo test -p ai-platform-capability-contract -p ai-platform-capability-worker` | Contract and worker lifecycle behavior is deterministic. |
| Database | `uv run --all-packages --extra test pytest -q --no-cov tests/database/test_agent_capability_execution_migration.py` | Fresh, upgrade, replay, scope, and sequence invariants hold. |
| Vertical slice | `make agent-capability-worker-gate` | A real Runtime fixture reaches Worker and closes exactly once. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- No write/unknown descriptor may be executable in this phase; it must remain hidden or return structured approval-required without dispatch.
