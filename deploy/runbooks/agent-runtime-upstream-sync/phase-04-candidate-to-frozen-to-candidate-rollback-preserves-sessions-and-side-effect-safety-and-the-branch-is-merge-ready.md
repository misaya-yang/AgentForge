# Phase 04 - Candidate-to-frozen-to-candidate rollback preserves sessions and side-effect safety, and the branch is merge-ready.

- PHASE_ID: ARU-04
- FEATURE_ID: ARU-F005
- DEPENDS_ON: ARU-03

## Outcome

The existing rollback authority switches candidate to frozen images and back without losing session history or repeating tool side effects, and final repository gates support merge readiness.

## Scope

In:

- Existing `agent-runtime-rollback-rehearsal`, final candidate chat, touched-path gates, and concise evidence handoff.

Out:

- Push, `main` merge, production deploy, broad cleanup, multi-architecture publication, or follow-up optimizations.

## Done when

- [ ] Candidate -> frozen -> candidate succeeds, prior sessions remain readable, and the execution ledger proves no duplicated side effect.
- [ ] One final candidate chat and all required touched-path/harness gates pass; every unrun item is reported as unverified.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Rollback | `make agent-runtime-rollback-rehearsal` | Existing sessions and side-effect receipts survive new -> old -> new. |
| Final repository gate | `make harness-check` | Repository control-surface contracts remain valid. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Do not push, merge `main`, or publish images without new explicit authorization.
