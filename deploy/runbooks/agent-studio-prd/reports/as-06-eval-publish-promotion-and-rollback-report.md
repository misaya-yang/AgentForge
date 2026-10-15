# AS-06 Eval, Publish, Promotion, and Rollback Actor Report

**Phase:** AS-06 — Eval, Publish, Promotion, and Rollback

**Feature:** AS-F007

**Status:** Passed — iteration-4 Critic approved; supported claim check exit 0

**Date:** 2026-07-19

**Actor:** primary implementation agent

## Outcome

AS-06 implements a closed release path from one exact saved Agent Draft to an
immutable Version and one atomic Publication pointer. The Gateway alone
resolves the Draft, model authorization, capability/Skill/Knowledge state,
runtime Snapshot and selected Eval Dataset content. Clients may request a
channel, Dataset, policy and operation, but cannot submit trusted spec,
fingerprint, profile, pass, score, threshold or waiver facts.

Actor iterations 2 through 4 address the preserved Critic findings:

- C-01: the Studio now queries the existing tenant-authorized Eval Dataset
  catalog, separate from Knowledge Datasets. The server binds Dataset ID,
  version and a canonical manifest hash over Dataset metadata and sorted full
  examples into the runtime, release and evaluation identities. Content or
  tenant drift fails closed.
- C-02: evaluation is a durable state machine. Creation persists `queued`, a
  separate execute endpoint claims `running`, completion writes `passed` or
  `failed`, and an Owner can cancel `queued` or `running`. PostgreSQL permits
  only valid transitions; cancellation can win completion and terminal
  evidence is immutable.
- C-03: a tenant+operation+idempotency-key advisory transaction lock is taken
  before Agent-specific release work. A real two-connection, two-Agent race
  produces one success and one stable `AGENT_RELEASE_IDEMPOTENCY_CONFLICT`,
  not raw 23505 or `AGENT_SLUG_CONFLICT`.
- C-04: evaluation list/detail recompute the current trusted candidate and
  return explicit stale reasons for Draft, runtime, release/evaluation
  identity, Eval Dataset and model/resource drift without mutating stored
  evidence.
- C-05/C-06: iteration 2 locked and rechecked persisted model/provider state,
  but its fresh Critic proved the database branch returned before invoking
  current provider readiness. Iteration 3 now requires that revalidator while
  the rows remain locked. Missing proof fails unverifiable; revoked readiness
  calls the callback once and leaves Publish at zero Version/Publication/
  event/request rows while Rollback preserves its pointer and all row counts.
- C-07: the iteration-3 Critic proved a concurrent Eval example update could
  commit after manifest resolution while Publish still committed. Iteration 4
  locks the Dataset parent `FOR UPDATE` and every manifest example `FOR SHARE`.
  Two real PostgreSQL cases pause Publish after the locked manifest and prove
  both update and insert remain blocked until the transaction commits.

All exact AS-06 required gates now exit zero with no required skips. A real
provider-free local browser run crossed Gateway and PostgreSQL for Dataset
creation/import, `queued→cancelled`, `queued→running→passed`, two immutable
promotions and one audited rollback. Gateway and Assistant were then restored
to `stub=false`; the same live path failed before Eval creation with
`AGENT_RUNTIME_MODEL_UNAVAILABLE`, proving the test switch and provider-free
claim did not leak into the normal runtime.

The iteration-4 independent Critic approved C-07 closure with no new material
finding. AS-F007 is now `passing`; AS-07 remains locked until the supported
phase claim check exits zero.

## Contract and Scope

- Fixed execution plan:
  `docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-plan.md`.
- Architecture deviation: none. Gateway remains the only Agent resolver;
  Assistant consumes the already validated runtime contract.
- The existing Eval/Trace platform, AS-02 session pinning, AS-03
  MCP/Connector authorization and AS-04 exact Skill/Knowledge bindings are
  extended or reused rather than duplicated.
- The obsolete unsupported Harness `--strict` option was skipped per user
  direction. No current test, migration, security, accessibility, rollback,
  Critic or completion gate was waived.
- Hosted delivery, Embed, external Runtime API, AS-08 deployment operations,
  commit, push and API-key changes are outside AS-06.

## Main Change Groups

| File/group | Required result |
| --- | --- |
| `database/migrations/077_agent_publication_eval.sql` | Initial forward-only release-evaluation, event, request and immutable publication evidence |
| `database/migrations/078_agent_release_lifecycle_hardening.sql` | Reentrant durable lifecycle, nullable in-flight completion time, Dataset/run tenant-composite FKs, Dataset/evaluation identity columns and transition trigger |
| `agent_version_candidate.py` | Prompt-free exact candidate, Dataset manifest and model authorization fingerprints, release/evaluation identities and truthful provider-free gate |
| `agent_repository.py` | Tenant Dataset manifest resolution with parent/example transaction locks, durable lifecycle, global key serialization, locked database identity plus current readiness revalidation, immutable Version, atomic promotion/rollback and explicit stale evidence |
| Agent schemas/routes/runtime | Additive queued/execute/cancel/list/detail/publish/rollback contracts and stable errors; provider-aware model lookup |
| Agent Web API/types/Studio | Separate Eval Dataset catalog, polling/cancel UI, stale reasons, truthful lifecycle and release surfaces |
| Required Python and browser tests | Real API, disposable PostgreSQL, concurrency, migration reentrancy, accessibility, responsive UI and live-stack evidence |
| AS-06 reports/matrix/JSON/screenshots | Durable prompt/Secret-free evidence; no generated credential or provider key |

No base Compose `build:` block, developer-machine absolute Docker path,
provider credential, public delivery route or unrelated Eval baseline behavior
was added in AS-06.

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 revision-bound evaluation | passed by Actor gates | Candidate binds Draft ID/revision/spec, complete runtime dimensions, model authorization and selected Dataset content; all detached/stale/content-mutation negatives pass |
| R2 idempotent immutable publication | passed by Actor gates | Same key/request replays original result; conflicting cross-Agent request is stable; one evaluated identity creates/reuses one sealed Version; evidence UPDATE/DELETE fails |
| R3 atomic promotion and rollback | passed by Actor gates | Real PostgreSQL failures, missing readiness proof, readiness revocation and model/resource races preserve prior pointer and every Version/Publication/event/request count; concurrent Eval example update/insert cannot cross the locked manifest decision; rollback requires exact channel history and current authorization |
| R4 truthful Studio controls | passed by Actor gates | Ten release cases plus 41-route regression cover lifecycle, Dataset, stale reasons, blocking findings, diff, publish, rollback, roles, desktop/mobile/dark, axe and network truth |

## Exact Required Validation

| Gate | Exact command | Final iteration-4 result |
| --- | --- | --- |
| Publish API/atomicity | `uv run pytest -q --no-cov tests/api/test_agent_publish_api.py tests/database/test_agent_publication_atomicity.py tests/services/eval/test_agent_publish_gate.py` | exit 0; 35 passed, 0 failed, 0 skipped (`14 API + 17 PostgreSQL + 4 gate`) |
| Agent Eval | `uv run pytest -q --no-cov tests/services/eval/test_agent_version_candidate.py && make eval-regression-gate && make verify-eval-dev` | exit 0; candidate 13; golden 16/16 with pass/critical/trajectory rate 1.0; existing groups 41/116/35/17; 0 skips |
| Frontend | `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-publish.spec.ts --config playwright.opensource.config.ts` | exit 0; lint 0 errors/17 inherited warnings; typecheck/i18n/build pass; browser 10/10, 0 skips |
| Runtime regression | `make verify-assistant-runtime-dev && make test-isolation` | exit 0; AHR 33/77/8/98 plus golden passed; ignored local account, explicit non-secret `qwen3.7-plus` model and temporary provider-free stub ran isolation 6/6 with 0 skips |

The two live isolation tests used an ignored local E2E account without printing
its password. Provider variables were explicitly blank in the temporary
containers, no external call was possible, and both Gateway and Assistant were
serially restored to `ASSISTANT_E2E_STUB_LLM=false` afterward.

The first iteration-4 isolation attempt is not counted as a pass: freshly
recreated release-image code returned two chat failures. After hot-syncing the
current source into the same provider-blank containers, the required rerun
passed 6/6 with zero skips; the formal services were then recreated with
`stub=false` and hot-synced again.

## Supplemental and Live Evidence

| Check | Final Actor result |
| --- | --- |
| Full open-source browser regression | `corepack pnpm@10.33.0 -C web e2e:opensource` -> first run exposed the AS-05 mock missing the new Eval Dataset read; after adding the exact empty catalog response, final run 41 passed, 0 failed, 0 skipped |
| Python static checks | Focused Ruff and Python compile invocations exit 0 |
| Migration execution | Local authorized development DB applied 078 once; migration table reports 74 applied and pending `(none)`; disposable PostgreSQL applies 077 and 078 twice |
| Compose ownership | Expected containers report `/Users/yang/projects/AI--Platfform` as `com.docker.compose.project.working_dir` |
| Low-memory operation | Services were hot-copied/restarted serially with no image build; final total around 0.78 GiB and temporary-test peak around 1.0 GiB, below the user-provided 3.5 GiB stop line |
| Live provider-free release | Remote Playwright 1/1: stable Eval Dataset/example, content hash, cancelled and passed lifecycles, two Versions/promotions, rollback to v1, three chronological audit operations and cleanup |
| Live restored fail-closed | Remote Playwright 1/1 after `stub=false`: exact model-unavailable alert and zero Eval/Version/Publication/event |
| Runtime final state | Eight repository-owned containers healthy; Gateway and Assistant both report `stub=false` |

Iteration 3 preserves its non-passing attempts. The first focused C-06 run
failed 2/2 because the PostgreSQL helper had not supplied the newly mandatory
readiness proof; after correcting only the helper, the focused run passed 2/2.
The first current-source live browser attempt failed before Agent work because
the harness bypassed Vite's same-origin proxy and lost browser auth; with
`VITE_API_BASE_URL=/`, the final provider-free flow passed 1/1. Neither failed
attempt is counted as a pass. The restored formal `stub=false` flow then passed
1/1 with exact model-unavailable and zero release evidence.

The host Node version is 24.14.0 while Web requests `^22.12.0`; pnpm emitted
the known engine warning. Docker release builds use Node 22. Vite also emitted
the inherited shared-chunk size warning. Neither warning caused an error or
skip.

## Browser and Security Evidence

- Durable matrix: `reports/agent-studio/as-06-eval-matrix.md`.
- Machine evidence: `reports/agent-studio/as-06-publish-atomicity.json`.
- Screenshots: `reports/agent-studio/as-06-screenshots/`.
- Exact browser suite passes 10/10 and full open-source suite passes 41/41.
  Axe finds zero serious/critical issues in covered release surfaces. The
  Cancel control received an explicit contrast-safe light/dark token rather
  than suppressing the accessibility finding.
- The live screenshot set now contains successful stale/publish/rollback and
  restored model-unavailable receipts in addition to deterministic
  desktop/mobile/dark/blocking states.
- Structured diff contains Prompt length/hash and changed paths, never Prompt
  body. Browser and audit evidence contain no signed Snapshot, credential,
  secret reference, raw tool result or server-only policy object.
- Publish/rollback reason patterns matching shared secret redaction are
  rejected before immutable persistence.

## Atomicity and Race Evidence

Migration 078 is additive and reentrant. It replaces the evaluation immutable
trigger with a lifecycle guard that allows only `queued→running/cancelled` and
`running→passed/failed/cancelled`, restricts mutable columns to lifecycle
fields, requires timestamps and denies terminal mutation/deletion. Tenant
composite FKs prevent cross-tenant Dataset/run evidence.

Promotion takes the global idempotency lock before Agent and publication
locks, replays a committed identical request, and rejects a changed request.
Inside the same transaction it checks exact passed evaluation identity,
current Draft/resources, locked Dataset manifest and locked model/provider
authorization, requires the current provider-readiness callback and compares
the complete proof before Version, pointer, event and request writes. Missing
or revoked readiness, a forced event failure and each injected model state
race leave every release count and pointer unchanged.

The locked Dataset manifest uses a parent `FOR UPDATE` lock plus `FOR SHARE`
locks on all selected examples. The parent lock serializes new imports through
their foreign-key check, while the example locks serialize update/delete; the
focused two-connection cases prove neither mutation commits in the paused
manifest-to-publication window.

Rollback follows the same idempotency and model/resource decision boundary,
requires the target to belong to the same Agent and exact Publication history,
invokes current readiness while the database authorization rows are locked,
and preserves the current pointer plus every release-row count for revoked,
unavailable, forbidden, current or never-targeted Versions. Successful
rollback never deletes a Version; existing sessions keep their pinned Version
and only new sessions resolve the new pointer.

## Eval Profile Boundary

`offline_v1` performs provider-free candidate integrity, current resource
readiness and secret-safety checks. It records duration, zero provider cost,
evaluator results and critical pass rate while explicitly setting
`model_quality_evaluated=false`. Binding a selected Dataset manifest is
release provenance, not a claim that an external model executed those cases.

Any configured production profile without approved Dataset, thresholds and
executor returns `AGENT_RELEASE_PROFILE_UNAVAILABLE`; there is no silent
fallback. Production quality/latency/cost thresholds and waiver governance
remain an AS-08 deployment input. No waiver or paid/live provider was used.

## Oracle, Critic, and Handoff

| Artifact/state | Current truth |
| --- | --- |
| `feature-oracle.json` | AS-F007 is `passing` from Actor evidence plus independent approval |
| `loop-state.json` | AS-06 iteration 4 is `verified` |
| iteration-1 verdict | preserved `as-06-critic-verdict-iteration-1.md`, `changes_requested`, findings C-01 through C-05 |
| iteration-2 verdict | preserved canonical and `as-06-critic-verdict-iteration-2.md`, `changes_requested`, C-06 provider-readiness bypass |
| iteration-3 verdict | preserved `as-06-critic-verdict-iteration-3.md`, `changes_requested`, C-06 closed and C-07 concurrent Eval manifest mutation reproduced |
| iteration-4 Actor evidence | focused manifest lock 2, exact required 35/candidate 13/frontend 10/runtime 6; prior supplemental 41 and both live paths remain recorded |
| iteration-4 verdict | canonical and preserved verdicts are identical and `approved`; C-07 closed, no new material blocker |
| supported claim check | exit 0, structure 100/100; validator explicitly checks metadata consistency only |
| next action | cold-start AS-07 / AS-F008 from its fixed Phase contract |

If the Critic requests changes, preserve its verdict and continue AS-06 only.
If it approves, the orchestrator may link the verdict, transition AS-F007,
write continuity/handoff facts and run the supported non-legacy claim check.
AS-07 is not unlocked before all of those steps.

No API key, provider token, database password, JWT, generated `.env` value,
Prompt body or signed Snapshot was printed, changed or copied into evidence.
No deployment, commit or push was performed.
