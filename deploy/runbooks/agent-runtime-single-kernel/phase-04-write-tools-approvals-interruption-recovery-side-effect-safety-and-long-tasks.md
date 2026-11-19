# Phase 04 - Write tools, approvals, interruption recovery, side-effect safety, and long tasks

- PHASE_ID: CHR-04
- FEATURE_ID: CHR-F005
- DEPENDS_ON: CHR-03

## Outcome

Every write or long-running call is policy-bound, approval-safe, idempotent, interruptible, and terminally paired.

## Scope

In:

- Tool lifecycle, approvals, dispatch ledger, Local Node, sandboxes, office writes, cancellation, crash recovery, and bounded task/subagent leases.

Out:

- Production canary, public V2 migration, or Python-loop deletion.

## Done when

- [ ] Every published call has exactly one result under cancellation, timeout, crash, disconnect, and approval resume.
- [ ] Unknown write effects are never blindly retried and approval cannot be replayed or widened.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Side-effect safety gate | `make agent-runtime-write-gate` | Pairing, approval, idempotency, sandbox, and recovery invariants all hold. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Shared external writes remain approval-gated; destructive fixtures must use isolated disposable state.
