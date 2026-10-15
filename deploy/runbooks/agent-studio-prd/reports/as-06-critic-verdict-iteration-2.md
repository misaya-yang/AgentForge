# AS-06 Independent Critic Verdict — Iteration 2

**Phase:** AS-06 — Eval, Publish, Promotion, and Rollback  
**Feature:** AS-F007  
**Critic:** fresh independent Critic subagent, iteration 2  
**Critic Verdict:** changes_requested  
**Actor Report:** docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md  
**Date:** 2026-07-19

## Inputs Reviewed

- Phase contract: docs/agent-studio-prd/phase-06-eval-publish-promotion-and-rollback.md
- Fixed plan: docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-plan.md
- Preserved iteration-1 verdict: docs/agent-studio-prd/reports/as-06-critic-verdict-iteration-1.md
- Actor evidence: docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md, reports/agent-studio/as-06-eval-matrix.md, and reports/agent-studio/as-06-publish-atomicity.json
- Implementation and tests: migrations 077/078; Agent candidate, repository, schemas, routes and runtime resolver; Agent Studio release UI; the four exact Phase test groups; live and deterministic browser specifications
- Independent work: source/transaction review, all four exact required gates, disposable real-PostgreSQL readiness reproducers, full open-source browser regression, source-fingerprint comparison, screenshot/dimension inspection, Compose ownership/health/memory checks, and git diff --check

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| C-06 | high | R3; current model/provider readiness; atomic promotion and rollback | The database model-authorization branch locks and rechecks the persisted model/provider rows, but it reconstructs the proof with runtime_provider_configured=true and returns before invoking the supplied readiness revalidator. A direct reproducer returned validation_result=accepted with readiness_revalidator_calls=0. In a disposable real PostgreSQL schema, a revalidator that would raise “provider readiness revoked” was never called: publish still committed Version/Publication/event/request counts 1/1/1/1; after two valid promotions, rollback also skipped the callback, changed the pointer and produced totals of 3 events and 3 requests. This violates the required zero-write behavior when current runtime readiness is revoked. | While holding the model/provider database locks inside the publish or rollback transaction, revalidate the current provider-configured/readiness state and compare a locked or versioned authorization proof. Any mismatch, exception or unavailable proof must fail closed before Version, pointer, event or request mutation. Add deterministic publish and rollback race tests that change readiness after pre-resolution and assert zero Version/Publication-pointer/event/request writes. |

Root cause:

- packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py:1943-1985 handles source=database, locks llm_models/llm_providers, hardcodes runtime_provider_configured=true at line 1979, and returns at line 1985.
- The callback path at agent_repository.py:1998-2002 is therefore unreachable for database-backed proofs.
- Promotion passes the callback at agent_repository.py:3106-3114 and rollback passes it at agent_repository.py:3674-3682, but both operations enter the early-return database branch.
- src/api/v1/agents.py:247-274 obtains provider readiness before the repository transaction. The closure at lines 277-294 is passed to publish and rollback, but it is not called by the database branch.
- Existing PostgreSQL model tests change is_enabled/access_level before the operation. They prove database-row reauthorization and rollback-on-failure, but do not exercise provider credential/readiness drift between pre-resolution and the atomic decision.

## Iteration-1 Finding Disposition

| Prior finding | Iteration-2 Critic conclusion |
| --- | --- |
| C-01 — wrong Dataset catalog and mutable ID-only identity | Closed. Studio now uses /api/v1/eval/datasets. The repository authorizes by tenant and hashes Dataset metadata/schema plus sorted full example content; candidate runtime, release and evaluation identities bind Dataset ID, version and manifest hash. Migration 078 adds tenant-composite Dataset/run foreign keys. Content drift is rejected by real PostgreSQL coverage. |
| C-02 — mocked cancellation and no real lifecycle | Closed. Create persists queued, execute atomically claims running, completion persists passed/failed, and Owner-only cancel handles queued/running. Row locks decide cancel-versus-complete; migration 078 permits only valid transitions and makes terminal evidence immutable. |
| C-03 — cross-Agent idempotency race/raw 23505 | Closed. A tenant+operation+key advisory transaction lock is acquired before Agent-specific locks. The real two-Agent PostgreSQL race yields one success and one typed AGENT_RELEASE_IDEMPOTENCY_CONFLICT with one Version/Publication/event/request. The release-request primary-key mapping is stable. |
| C-04 — Draft-only stale truth | Closed. API list/detail rebuild the current server candidate for passed evidence and compares Draft, runtime, release, evaluation and Dataset identities. Model/Skill/MCP/Knowledge unavailability fails closed through resolver error reasons; otherwise composite resource drift is exposed as runtime_fingerprint_changed. The generic reason is a non-blocking evidence caveat, not a release-integrity gap. |
| C-05 — model authorization outside the transaction | Partially closed. Persisted model/provider identity, enabled state, access level and catalog timestamps are now locked and rechecked. C-06 shows that external provider-configured/readiness state is still only checked before the transaction and is not revalidated by the database branch, so the full finding is not closed. |

## Requirement Coverage

- **R1 — revision-bound evaluation:** Supported. The saved Draft ID/revision/spec, complete runtime fingerprint, model authorization token, Eval Dataset version/content manifest, release identity and evaluation identity are server-resolved and persisted. Client-owned pass/profile/fingerprint fields are rejected. Draft, runtime and Dataset drift fail closed.
- **R2 — idempotent immutable publication:** Supported. Global key serialization, stable replay/conflict, immutable Version/evaluation/event/request triggers, forced event rollback, and cross-Agent concurrency pass against PostgreSQL.
- **R3 — atomic promotion and rollback:** Not yet supported in full. Database row revocation/access races, pointer/event/request atomicity, rollback history and recoverability pass, but C-06 permits publish and rollback after current provider readiness has been revoked.
- **R4 — truthful Studio controls:** Supported for the reviewed surfaces. Lifecycle, Dataset provenance, provider-free scope, diff, blocking state, roles, immutable history, session-pinning notice and rollback are rendered from server evidence. The UI does not claim external-model quality.

AS-F007 must remain failing and AS-07 must remain locked until C-06 is corrected and independently re-reviewed.

## Exact Required Gates

1. uv run pytest -q --no-cov tests/api/test_agent_publish_api.py tests/database/test_agent_publication_atomicity.py tests/services/eval/test_agent_publish_gate.py  
   Exit 0: 31 passed, 0 failed, 0 skipped; one Starlette deprecation warning.

2. uv run pytest -q --no-cov tests/services/eval/test_agent_version_candidate.py && make eval-regression-gate && make verify-eval-dev  
   Exit 0: candidate 13 passed; golden 16/16 with pass, critical-pass and trajectory-pass rates 1.0; inherited Eval groups 41, 116, 35 and 17 passed; 0 required skips. Web lint/type-check invoked by the gate passed with 0 errors and 17 inherited warnings.

3. corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-publish.spec.ts --config playwright.opensource.config.ts  
   Exit 0: lint 0 errors/17 warnings; type-check, i18n and build passed; release Playwright 10 passed, 0 failed, 0 skipped. Node 24 versus requested Node 22 and the inherited large-chunk warning remain non-failing warnings.

4. make verify-assistant-runtime-dev && make test-isolation  
   The first credentialed provider-blank run was not counted as a pass: AHR 33/77/8/98 plus golden passed, but isolation exited 2 with 4 passed, 2 failed, 0 skipped because the provider-blank model catalog was empty. The full exact group was rerun with the explicit non-secret ASSISTANT_ISOLATION_MODEL=qwen3.7-plus override against temporary provider-free stub Gateway/Assistant containers. Final exit 0: AHR 33/77/8/98 plus golden passed; isolation 6 passed, 0 failed, 0 skipped.

Supplemental corepack pnpm@10.33.0 -C web e2e:opensource independently passed 41/41 with 0 skips. Actor live receipts for provider-free lifecycle/two promotions/rollback and restored model-unavailable failure were source- and screenshot-reviewed; they were not re-executed by this Critic after C-06 was demonstrated.

## C-06 Reproducer Evidence

The direct check called DatabaseAgentRepository._validate_model_authorization with a valid database proof and a current-readiness callback that returned unavailable:

- validation_result=accepted
- readiness_revalidator_calls=0

The disposable PostgreSQL reproducer applied the same 071/077/078 schema and used real repository transactions:

- publish_readiness_revalidator_calls=0
- publish_counts_after_revoked_readiness=versions:1,publications:1,events:1,requests:1
- rollback_readiness_revalidator_calls=0
- rollback_pointer_changed=true
- rollback_event_count=3
- rollback_request_count=3
- rollback_result_pointer=true

The expected result was a stable readiness error and no Version, pointer, event or request change.

## Source Fingerprints and Diff

reports/agent-studio/as-06-publish-atomicity.json contains 12 frozen source fingerprints. Independent SHA-256 comparison matched 12/12:

| Source | SHA-256 |
| --- | --- |
| database/migrations/077_agent_publication_eval.sql | 8eeefb2177d766291a3b12fe4b23abec258a9f6b5b2dc84f83f740045432bbb4 |
| database/migrations/078_agent_release_lifecycle_hardening.sql | 3cf4d70391258fe60f73aa70725b9e6edcc8090b9d584f42cc081c3c081e7969 |
| agent_version_candidate.py | bcbff8f7533da0fec22a1b289ba4f6b6e9c0756b37a3999d44d1db18a44da170 |
| agent_repository.py | eb44186910828f691e7caf91b8392c64e8137ef083062cbb56cbdd43f1a5af4d |
| src/api/schemas/agents.py | f69860cbb1bbb4631c4c8acac5dca141bd0a2b16748e9aea1306ca7575450a7b |
| src/api/v1/agents.py | e9600c3ecbaf10ba9285b087023d7aaaf2fbdcc4979a67cb2f80eb925d295c23 |
| src/api/v1/agent_runtime.py | 399782e522f847bdfd80509f7e9ca0db4fcf99ac377172c24f06b5d108dbc1aa |
| AgentReleasePanel.tsx | a9d4ac627276cde18033cfe780abd2c6e1cabab1d1136cbe61fcee0b8f7aca6a |
| AgentStudioPage.tsx | c56e544c19de35c5210a91710700028c5fac5abccc94219e83a4cdf4d20ef5b9 |
| web/e2e/agent-publish.spec.ts | bf5df17ae1a278a813cc428908940e24db59e814aa30ed7498f28f8353db4e47 |
| web/e2e/agent-publish-live.spec.ts | 2f9804c11ebb83cf0805eb90457fdefd6484fd6aa4aa161919e66bf5b7f27ac1 |
| web/e2e/agent-studio.spec.ts | 1310615c19cc342c53d7b0c075cd8bbb328efc7e884baa52ec29f1d689b2a012 |

git diff --check exited 0. The checkout contains extensive pre-existing and earlier-phase changes, so this verdict approves no unrelated diff. The Critic made no implementation, test, Actor evidence, Oracle, loop-state, migration, deployment or secret change; only this verdict and its identical iteration copy are intentional writes.

## Security, Privacy, Eval Truth, and Failure Assessment

Tenant and Agent ACLs, Owner-only mutations, server-owned candidate/profile/gate facts, composite Dataset integrity, Secret-shaped reason denial, prompt-free structured diff, immutable release evidence and stable idempotency errors are sound in the inspected paths. Structured Prompt evidence contains hashes/lengths and never the Prompt body. No credential, API key, signed Snapshot, raw tool output or generated .env value was printed or copied into evidence.

offline_v1 truthfully sets model_quality_evaluated=false, records zero provider cost and labels its scope as provider-free release integrity. An unconfigured production profile fails closed; no production-quality, threshold, paid-provider or external-provider success claim is accepted.

C-06 is an authorization-freshness defect, not merely a test-coverage issue: the demonstrated transactions commit after the current readiness authority says the provider is unavailable.

## Browser and Runtime Assessment

The exact release suite passed desktop/mobile lifecycle, selected Eval Dataset, real cancel endpoint, blocking state, dark mode, stale Draft, idempotent publish, session pinning, rollback history and Owner/Editor/Viewer behavior. Axe, keyboard/focus, overflow and happy-path console/network assertions are part of the passing suite.

Fifteen AS-06 screenshots were enumerated and their raster dimensions checked, including 1440x900, 390x844, full-page mobile, dark Publish, live model-unavailable and live rollback audit states. Direct visual inspection of representative desktop, mobile, dark and live rollback captures found no material UI-truth contradiction. Screenshots cannot establish the missing readiness transaction boundary in C-06.

Before Docker work, all eight ai-gateway containers were healthy and labeled with compose working_dir=/Users/yang/projects/AI--Platfform. Temporary provider-free Critic containers kept provider credentials blank, stayed below the 3.5 GiB ceiling, and were removed. Final state: eight repository-owned containers healthy, Gateway and Assistant both stub=false, observed total memory approximately 761 MiB.

## Minimal-Change, Rollback, and Handoff Assessment

The AS-06 implementation is concentrated in the planned migration, candidate/repository/API, Agent Studio release UI, locales, tests and evidence. Immutable Version history, prior-pointer recoverability, historical-target enforcement, revocation denial, append-only audit and existing-session pinning are materially implemented.

Rollback cannot receive release approval while the same readiness gap can move its pointer and append event/request rows. The next Actor iteration must remain in AS-06, correct C-06, rerun all four exact zero-skip gates, add the focused publish/rollback readiness zero-write tests, freeze new fingerprints, and request a fresh independent Critic. This verdict does not mutate the Oracle or unlock AS-07.

## Whole-Demand Regression Assessment

Inherited Eval, Assistant runtime, isolation and full open-source browser regressions passed. AS-09 same-build whole-demand regression remains pending. No AS-09, deployment, Hosted, Embed, Runtime API, commit or push claim is made.

## Verdict Rationale

**changes_requested.** C-01 through C-04 are closed, and the database-row portion of C-05 is materially improved. However, C-06 independently proves that the transaction does not consult the current provider-readiness authority for database-backed model proofs. Both promotion and rollback commit exactly the state that the Phase requires to remain unchanged. Green required gates therefore do not justify AS-F007 completion until the readiness proof participates in the atomic decision and new zero-write race tests pass under fresh independent review.

