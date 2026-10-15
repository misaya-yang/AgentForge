# AS-06 Eval, Publish, Promotion, and Rollback Plan

- **Phase:** AS-06 — Eval, Publish, Promotion, and Rollback
- **Feature Oracle:** AS-F007 only
- **Status:** fixed Phase-contract execution record; Actor iteration 1 in progress
- **Date:** 2026-07-19
- **Scope rule:** execute the existing AS-06 contract without replanning, shrinking, or entering AS-07 Hosted/Embed/Runtime API delivery

This artifact transcribes the approved AS-06 Phase into an execution record.
It does not change the product architecture, dependency order, acceptance
gates, AS-07 channel boundary, or AS-09 whole-demand standard.

## Dependency and Baseline

- AS-05/AS-F006 is `passing` after two preserved `changes_requested`
  reviews, fresh iteration-3 approval and an exit-zero supported claim check.
  Its exact Draft revision state, atomic save, immutable Version Preview,
  isolated sessions and responsive Studio fixtures are the AS-06 input.
- Migration 071 already provides immutable Version/binding rows, one
  Publication per Agent/channel and append-only publish events. The current
  `create_version` validates and seals a Draft but has no Eval gate,
  idempotency identity or atomic promotion/rollback API.
- The existing Eval platform supports Datasets, live candidates, evaluator
  jobs, actual runtime fingerprints, critical-pass gating, golden regression
  and trace comparisons. It is generic and currently accepts target snapshot
  configuration; an Agent release candidate must instead be resolved entirely
  from tenant/ACL-authorized saved server state.
- Repository-owned Compose is 8/8 healthy around 763 MiB. Docker/migration
  work remains serial under the 3.5 GiB stop line. API keys, paid/live model
  evaluation, deployment, commit and push remain unauthorized.

## Open-Source Eval Profile Boundary

No approved production Eval Dataset/quality thresholds or paid-model execution
input exists. AS-06 therefore implements a server-owned, versioned,
non-secret release-profile contract rather than hardcoding deployment-specific
numbers:

- deterministic development evidence uses a provider-free `offline_v1`
  profile and existing golden/trace fixtures;
- tenant/auth/Secret/safety/resource readiness, stale revision/fingerprint,
  incomplete/error trials, immutable Version, idempotency and transaction
  integrity are always blocking and cannot be waived;
- configured blocking evaluators must pass, every critical case must pass and
  the run must contain one complete verified runtime fingerprint;
- latency and cost evidence must be recorded truthfully, but AS-06 does not
  invent production budgets before the AS-08 deployment baseline;
- a production profile without configured approved Dataset/threshold input
  remains unavailable and fails closed. The client cannot select `offline_v1`
  to bypass a server-selected production policy.

This is provider-free implementation/eval evidence, not production-model
quality or deployment readiness.

## Requirement-to-Change Map

| Contract | Bounded implementation | Primary evidence |
| --- | --- | --- |
| R1 revision-bound evaluation | Add a tenant/Agent-scoped release-evaluation record whose server-resolved candidate binds Draft ID/revision, spec hash, resolved runtime/prompt/tool/Skill/Knowledge fingerprints, selected Eval Dataset/profile and actual Eval run evidence. Recheck all fields at publish time and mark changed Drafts stale. | API/service tests, offline golden candidate, stale-revision/fingerprint negatives |
| R2 idempotent immutable publication | Add request idempotency key + canonical request hash around exact Draft-to-Version materialization. Same key/payload returns the original immutable Version/event; a changed payload returns stable conflict. Never update a Version or sealed binding. | PostgreSQL uniqueness/immutability/rollback tests and API replay/conflict cases |
| R3 atomic promotion and rollback | In one transaction lock Owner/Agent/Publication, reauthorize current MCP/Connector/Skill/Dataset/model resources, create or reuse the exact Version, update the channel pointer/status and append the validation-bearing audit event. Rollback uses the same path against a historical healthy Version and preserves the previous pointer on failure. | real PostgreSQL trigger/failure tests, pointer/event invariants, current-revocation negatives |
| R4 truthful Studio controls | Extend Agent Studio with Eval state/history, structured saved-Draft-to-Version diff, blocking/non-blocking findings, Publish Sheet, Version list/diff and rollback confirmation. Server responses own all pass/stale/permission/idempotency facts; desktop/mobile keyboard/axe/console/network evidence is durable. | `agent-publish.spec.ts`, exact 1440x900/390x844 screenshots, route/static regression |

## Bounded File Groups

- Forward-only Agent release/eval migration and existing Agent repository/API
  schema/route registration.
- Existing Eval candidate/executor/repository seams only where required to
  accept a closed server-resolved Agent target and persist exact evidence;
  unrelated baseline promotion semantics remain unchanged.
- `web/src/pages/agents`, typed Agent API/types/locales and the new
  `web/e2e/agent-publish.spec.ts`; existing `/eval` and `/assistant` behavior
  changes only through regression-safe shared contracts.
- Phase tests named by the contract:
  `test_agent_publish_api.py`, `test_agent_publication_atomicity.py`,
  `test_agent_publish_gate.py`, `test_agent_version_candidate.py`.
- AS-06 report, eval matrix, atomicity JSON, screenshots and required Harness
  writeback only.

## Fixed Execution Sequence

1. Run pre-change Eval/runtime/Compose ownership and memory baselines; record
   existing gate counts and any failures without relabeling them.
2. Add the forward migration for exact Agent release-evaluation evidence,
   server-owned release profile identity, idempotency request/hash/result and
   database constraints/triggers that preserve immutable Versions, append-only
   events and one atomic Publication pointer.
3. Implement closed Agent candidate resolution and evaluation orchestration:
   authorize Owner, lock/read the saved Draft, resolve current resource/model
   state, persist exact requested/actual fingerprints, execute provider-free
   fixtures, expose queued/running/passed/failed/cancelled/stale states and
   forbid client-declared pass or protected target overrides.
4. Implement repository/API validation, deterministic structured diff,
   idempotent Version creation, atomic promotion and rollback. Recheck the
   exact Eval, Draft revision/spec, current resource authorization and health
   inside the transaction; preserve prior pointers/events on every failure.
5. Extend the Studio with Eval, Publish and Versions routes/surfaces using the
   existing Agent design system. Cover no-dataset, lifecycle, stale,
   validation/resource/permission/error, diff, idempotent retry, successful
   promotion, Draft-after-publish and rollback/session-pinning states.
6. Add provider-free golden, API, migration/transaction and browser evidence;
   run axe, real keyboard/focus, console/network and exact 1440x900/390x844
   rendered inspection. Preserve existing Eval, Agent Preview and Assistant.
7. Run all four exact Phase gate groups with zero skips, write durable Actor
   evidence, freeze the source and request a fresh independent Critic. Keep
   AS-F007 `failing` and AS-07 locked until approval plus supported claim check.

## Required Validation Gates

| Gate | Exact Phase command | Required outcome |
| --- | --- | --- |
| Publish API/atomicity | `uv run pytest -q --no-cov tests/api/test_agent_publish_api.py tests/database/test_agent_publication_atomicity.py tests/services/eval/test_agent_publish_gate.py` | Revision/fingerprint, stale, idempotency, immutable row, transaction rollback, promotion/audit and rollback pass with zero skips. |
| Agent Eval | `uv run pytest -q --no-cov tests/services/eval/test_agent_version_candidate.py && make eval-regression-gate && make verify-eval-dev` | Closed Agent candidates preserve exact fingerprints; provider-free golden and existing Eval gates pass. |
| Frontend | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-publish.spec.ts --config playwright.opensource.config.ts` | Static and Publish/Diff/Eval/Version/Rollback browser gates pass with zero skips. |
| Runtime regression | `make verify-assistant-runtime-dev && make test-isolation` | Existing Assistant/runtime isolation remains green without changing published/preview session identity. |

## Browser, Security, and Rollback Matrix

- `/agents/:id/evals` at 1440x900 and 390x844: no Dataset, queued, running,
  passed, failed, cancelled, stale revision, retry and permission states.
- Publish Sheet at both viewports: exact source revision, structured
  identity/prompt/model/capability/Skill/Knowledge diff, resource checks,
  blocking/non-blocking findings, Eval links, disabled submit, idempotent
  retry and successful promotion.
- `/agents/:id/versions`: immutable list/detail/diff, current channel pointer,
  Draft-after-publish non-drift, rollback confirmation/revocation/failure,
  previous pointer recoverability and existing-versus-new session messaging.
- Owner executes Eval/publish/rollback; Editor/Viewer cannot cross server
  mutation seams. No hidden control is an authorization boundary.
- Axe zero critical/serious; Tab/Shift+Tab/Enter/Space/Escape, focus return,
  exact viewport labels, overlay containment, no horizontal overflow and clean
  happy-path console/network receipts are required.
- Secret, Prompt body, signed Snapshot, raw tool result and credential values
  never enter Eval/publication browser state, audit, idempotency or evidence
  payloads.

## Stop Conditions and Rollback

- Stop if Agent candidate identity can be client-supplied, Eval evidence is
  detached from exact saved state, current authorization is not rechecked,
  Version/idempotency/audit immutability is not database-backed, or
  pointer/event updates cannot share one transaction.
- Stop if a paid/live provider or production Dataset/threshold is required;
  request explicit approval rather than reading/inventing API keys or numbers.
- Rollback disables publish mutations while retaining all read/audit data,
  then atomically repoints an affected channel to the last known healthy,
  currently authorized Version and appends an incident event. No immutable
  Version or event is deleted.
- AS-07 Hosted/Embed/Runtime API, production migration/deployment, commit and
  push are explicitly excluded.
