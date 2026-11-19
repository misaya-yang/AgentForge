# Phase 05 - Lazy legacy-session import, Docker/browser acceptance, and stable canary rollout

- PHASE_ID: CHR-05
- FEATURE_ID: CHR-F006
- DEPENDS_ON: CHR-04

## Outcome

Legacy sessions import exactly once and real Docker/browser/eval evidence supports stable canary promotion and rollback.

## Scope

In:

- Transactional lazy import, legacy projection, runtime assignment, Docker validation, authenticated browser journeys, quality/performance cohorts, and canary controls.

Out:

- Public V2 removal of V1 or deletion of the control loop.

## Done when

- [ ] Quiescent legacy sessions import atomically; in-flight sessions never switch kernel mid-Turn.
- [ ] Full browser, Docker, Agent Eval, latency, security, and rollback gates pass before each canary step.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Migration and canary gate | `make agent-runtime-canary-gate` | Import receipts, real product journeys, cohorts, performance, and rollback rehearsal pass. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

### New-session assignment controls

The Gateway chooses an owner once, when a session is first created. The bucket
is `sha256(salt:tenant_id:session_id) % 100`; prompt text, model, response,
tool names, and turn results are never inputs. Existing rows in
`assistant_session_runtime_assignments` are immutable.

```text
ASSISTANT_RUNTIME_CANARY_PERCENT=0|1|10|25|50|100
ASSISTANT_RUNTIME_CANARY_E2E_TENANTS=tenant-a,tenant-b
ASSISTANT_RUNTIME_CANARY_KILL_SWITCH=false
ASSISTANT_RUNTIME_CANARY_SALT=ai-platform-runtime-v1
```

The explicit E2E tenant override and kill switch affect only new sessions;
active Agent turns finish on their original owner. Rollback therefore means
new sessions select `python_control` while existing assignments remain pinned.
The CHR-06 deletion guard additionally requires a completed 100% window, zero
legacy-loop calls, V1 compatibility, and a passing assignment rollback
rehearsal. No code gate treats those conditions as production evidence.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Any production percentage change, shared migration, or rollout requires explicit deployment authorization.
