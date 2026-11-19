# AS-06 Eval, Publish, and Rollback Matrix

- Phase: AS-06 — Eval, Publish, Promotion, and Rollback
- Feature: AS-F007
- Date: 2026-07-19
- Status: passed — Actor iteration 4 gates and independent Critic approved
- Release profile: server-owned `offline_v1` (`provider_free_release_integrity`)
- Production quality boundary: not evaluated and not claimed

## Server-Owned Eval Matrix

| Contract/state | Server truth exercised | Result and evidence |
| --- | --- | --- |
| Exact saved target | Gateway resolves tenant/Agent, Draft ID/revision/spec hash, exact model authorization, Prompt hash, capability/Skill/Knowledge/tool/runtime/Snapshot/channel-policy fingerprints and selected Eval Dataset manifest; clients cannot submit trusted spec, fingerprints, profile, pass, score, threshold, or waiver facts | Candidate 13/13 and API negatives reject detached tenant/Agent/channel/auth/policy/spec, incomplete fingerprints, silently dropped resources, Dataset tenant mismatch and model-catalog mismatch |
| Authorized Eval Dataset | Studio queries `/api/v1/eval/datasets`, not Knowledge Datasets. The server authorizes the tenant Dataset, locks its parent and manifest examples, and hashes version/schema/metadata plus sorted full examples into one immutable manifest | Real API, PostgreSQL content-mutation plus concurrent update/insert serialization, and browser selector tests pass; live stack binds Dataset ID, version and 64-character manifest hash |
| No Dataset | Missing optional Eval Dataset is a named non-blocking finding for `offline_v1`; result explicitly states model quality was not evaluated | `AGENT_EVAL_DATASET_NOT_SELECTED` and provider-free scope render on desktop/mobile without a quality claim |
| Queued | `POST /evals` stores a durable queued row and sequence-1 event after exact current model/resource/Dataset checks | API, PostgreSQL and live browser assertions pass |
| Running | Only `POST /evals/{id}/execute` may claim queued→running; a second executor cannot claim the same row | Repository lifecycle and terminal-race tests pass |
| Cancelled | Owner can cancel queued or running evidence; cancellation writes a terminal event, can win completion, is idempotently observable, and can never publish | Real API cancel endpoint, PostgreSQL transition trigger, deterministic browser and live queued→cancelled flow pass |
| Passed | Completion rechecks current Draft, runtime, model authorization and Dataset manifest; every release-integrity evaluator passes and critical pass rate is 1.0 | API/gate/PostgreSQL/browser and real live queued→running→passed flow pass |
| Failed/blocking | Missing/tampered identity or resource drift creates a server-owned blocking result; Publish stays disabled | Gate tests plus `publish-server-blocked-1440x900.png` |
| Stale | List/detail recompute the current candidate. Draft, runtime, release, evaluation, Dataset or model/resource changes return explicit stale reasons without mutating evidence | API runtime-drift test, PostgreSQL Dataset/model races and `eval-stale-after-draft-save-1440x900.png` pass |
| Production profile unavailable | Any configured non-`offline_v1` profile remains unavailable until deployment supplies approved Dataset, thresholds and executor; client cannot choose a fallback | Unit/API tests return `AGENT_RELEASE_PROFILE_UNAVAILABLE` |
| Missing model in restored live stack | With `stub=false` and no usable configured Qwen provider, candidate resolution fails before creating Eval evidence | Remote Playwright 1/1 returns `AGENT_RUNTIME_MODEL_UNAVAILABLE` with zero Eval/Version/Publication/event |

The provider-free profile binds selected Dataset content but does not claim to
execute external-model quality cases. Existing golden, trajectory, tool, RAG,
safety, latency and cost infrastructure is separately covered by the 16/16
golden gate and `make verify-eval-dev`. Production Agent thresholds remain a
deployment input.

## Publish and Rollback Matrix

| Operation | Positive contract | Negative/atomic contract | Evidence |
| --- | --- | --- | --- |
| Publish | Owner supplies evaluation ID, reason and `Idempotency-Key`; server re-resolves exact candidate, locks model/Dataset/examples/resources, invokes current provider-readiness authorization, creates or reuses one immutable Version, moves one channel pointer, appends one event and records one request in one transaction | Concurrent example update/import cannot cross the manifest decision; missing readiness proof, readiness revocation, missing key, Viewer/Editor, stale/non-passed Eval, Dataset/model/resource drift, secret-shaped reason and forced event failure produce zero partial writes | API 14, real PostgreSQL 17, gate 4 and browser publish paths |
| Concurrent idempotency | Advisory transaction lock reserves tenant+operation+key before Agent-specific work | Same key raced across different Agents produces exactly one success and one stable conflict, never raw 23505 or misleading slug conflict | Two-connection real PostgreSQL race plus API constraint mapping |
| Replay | Same tenant/operation/key/request returns original Version/Publication/event with `idempotent_replay=true` | Changed canonical request returns `AGENT_RELEASE_IDEMPOTENCY_CONFLICT`; committed replay remains available after mutable Draft/resource change | API plus PostgreSQL replay tests |
| Rollback | Owner chooses a non-current Version previously targeted by that Publication; server rebuilds runtime, locks/rechecks model/resources and invokes current provider-readiness authorization before moving pointer/audit | Missing or revoked readiness, disabled/forbidden model, revoked/current/foreign/unreleased/non-historical target leaves pointer and every Version/Publication/event/request count unchanged | PostgreSQL readiness/model races and history/revocation tests; disabled UI action |
| Draft after publish | Draft remains independently mutable while immutable Version and current pointer are stable | Browser never presents Draft edits as production | Version/current-target UI and DB immutability tests |
| Session behavior | Existing AS-02 sessions retain their pinned Version; only new sessions resolve current pointer | Promotion/rollback never mutates historical Version/session identity | Runtime regression and explicit UI notice |

Machine-readable evidence: `reports/agent-studio/as-06-publish-atomicity.json`.

## Browser, Accessibility, and Live Matrix

| Surface | State/interaction | Result | Durable evidence |
| --- | --- | --- | --- |
| `/agents/:id/evals` | no Dataset; queued, running, passed, failed, cancelled, stale | Every state named; only current passed evidence can open Publish | lifecycle desktop/mobile/full-page captures |
| Eval Dataset selector | authorized versioned Eval catalog | Selected ID is sent to server and returned with version/manifest | deterministic selector test plus real live Dataset |
| Eval history | Draft/runtime changes after pass | visibly stale with explicit reasons and no Publish | stale screenshot and API browser case |
| Publish Sheet | exact identity/profile/resource checks and prompt-free diff | client cannot forge pass; Prompt body absent | desktop/mobile/dark captures; blocking submit case |
| Version history | immutable Versions, current pointer, unreleased target | current target and history explicit; never-published Version cannot roll back | versions/audit capture and E2E negative |
| Rollback | confirmation, reason, pointer movement and append-only event | pointer moves to v1; v2 remains recoverable; audit visible | deterministic and live rollback captures |
| Real provider-free stack | stable Eval Dataset, cancel, pass, two promotions, rollback | all operations cross real Gateway and PostgreSQL; no external provider call | live remote Playwright 1/1 |
| Restored no-provider stack | model unavailable | stable 503, zero release evidence, cleanup passes | live model-unavailable capture; remote Playwright 1/1 |
| Owner/Editor/Viewer | inspect versus mutate | Owner mutates; Editor/Viewer inspect without mutation request | parameterized browser tests |

The exact release browser suite passed 10/10. It runs axe, keyboard/focus,
overflow and happy-path console/network checks. The full open-source browser
regression passed 41/41 after the AS-05 Harness registered the new read-only
Eval Dataset request. All named viewport screenshots have matching raster
dimensions.

## Required and Supplemental Gates

| Gate | Final Actor iteration-4 result |
| --- | --- |
| Publish API/atomicity | 35 passed, 0 failed, 0 skipped (`14 API + 17 PostgreSQL + 4 gate`) |
| Agent candidate | 13 passed, 0 failed, 0 skipped |
| Eval golden/regression | 16/16 golden; existing groups 41/116/35/17; zero skips |
| Frontend required | lint 0 errors/17 inherited warnings; typecheck, i18n and build passed; release browser 10/10 |
| Full route regression | 41/41 Playwright cases passed |
| Assistant runtime | groups 33/77/8/98 plus golden passed |
| Isolation | 6/6 real Gateway→Assistant HTTP tests passed, zero skips |
| Migration | 74 applied including 078, zero rollback records, zero pending |
| Live release boundary | provider-free success 1/1 and restored fail-closed 1/1 |
| Runtime state | eight repository-owned containers healthy around 0.78 GiB; Gateway and Assistant restored `stub=false`; host/container repository hashes match |

Iterations 3 and 4 truthfully preserve three discarded attempts: the first focused
C-06 run failed 2/2 until the test helper supplied the newly required proof,
and the first live browser run failed before Agent work because its harness
bypassed the same-origin proxy. The corrected focused run passed 2/2 and the
corrected live provider-free flow passed 1/1. The first iteration-4 isolation
run failed 2/6 on recreated release-image code; current-source hot-sync made
the required rerun pass 6/6. None of the failed attempts is counted as a pass.

No provider credential, API key, local database password, JWT, signed Runtime
Envelope, Prompt body or generated `.env` value is present here or in the
screenshots. No production-model quality, external-provider success, Hosted,
Embed, Runtime API, deployment, commit, push or AS-09 completion is claimed.
