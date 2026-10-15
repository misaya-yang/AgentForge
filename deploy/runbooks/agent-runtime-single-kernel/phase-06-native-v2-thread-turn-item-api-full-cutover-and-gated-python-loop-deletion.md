# Phase 06 - Native V2 Thread/Turn/Item API, full cutover, and gated Python loop deletion

- PHASE_ID: CHR-06
- FEATURE_ID: CHR-F007
- DEPENDS_ON: CHR-05

## Outcome

Every product surface uses the native V2 Thread/Turn/Item contract and the legacy Python loop is demonstrably unused before deletion.

## Scope

In:

- V2 API/SSE/SDK/Web, V1 deprecation window, full traffic cutover, rollback rehearsal, and evidence-gated legacy deletion.

Out:

- Unrelated Gateway, Knowledge, UI, or provider refactors.

## Done when

- [ ] All surfaces consume V2 with native Item fidelity and V1 remains compatible for one release window.
- [ ] 100% canary stability, zero legacy calls, deletion review, and rollback evidence exist before removing Python orchestration.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Final cutover gate | `make agent-runtime-cutover-gate` | V2 parity, deprecation, zero legacy traffic, rollback, and deletion requirements pass. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Deleting the old loop, removing V1, committing, pushing, or publishing requires explicit owner authorization.
