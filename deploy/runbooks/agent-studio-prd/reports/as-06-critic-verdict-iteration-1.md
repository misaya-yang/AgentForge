# AS-06 Independent Critic Verdict

**Phase:** AS-06 — Eval, Publish, Promotion, and Rollback  
**Feature:** AS-F007  
**Critic:** fresh independent Critic subagent  
**Critic Verdict:** changes_requested  
**Actor Report:** `docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md`  
**Date:** 2026-07-19

## Inputs Reviewed

- Phase contract: `docs/agent-studio-prd/phases/phase-06-eval-publish-promotion-and-rollback.md`
- Oracle and loop state: `docs/agent-studio-prd/feature-oracle.json`, `docs/agent-studio-prd/loop-state.json`
- Fixed Actor plan and report: `docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-plan.md`, `docs/agent-studio-prd/reports/as-06-eval-publish-promotion-and-rollback-report.md`
- Implementation: migration 077; Agent release candidate, repository, API schemas/routes, runtime resolver, Web client/types/release panel/Studio composition, locales, and release Playwright harness
- Tests: the four required Python files and `web/e2e/agent-publish.spec.ts`, plus the inherited Eval and Assistant-runtime gates
- Durable evidence: `reports/agent-studio/as-06-eval-matrix.md`, `reports/agent-studio/as-06-publish-atomicity.json`, and all 12 PNGs under `reports/agent-studio/as-06-screenshots/`
- Independent inspection: targeted source/diff review, two disposable-schema PostgreSQL reproducers, exact four validation groups, source-fingerprint comparison, PNG raster/pixel checks, Compose ownership/health/memory checks, and `git diff --check`

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| C-01 | high | R1, R4, AS-F007 selected-Dataset flow | The release selector loads **Knowledge** datasets from `/api/v1/knowledge/datasets` (`web/src/api/agents.ts:24,246-248`) and submits their `dataset_id` (`AgentReleasePanel.tsx:250-262,370-373`), while persistence accepts only an ID present in `eval_datasets` (`agent_repository.py:1831-1843`) and migration 077 references that table. The Playwright route mocks both the Knowledge catalog and POST Eval, so its selected-Dataset case never crosses this boundary (`agent-publish.spec.ts:313-320,467-469`). An independent real-PostgreSQL reproducer inserted a tenant Knowledge dataset and received `AGENT_EVAL_DATASET_NOT_FOUND`; inserting a same-UUID `eval_datasets` row changed the result to `passed`. In addition, the candidate binds only mutable `dataset_id`; no Eval Dataset version/content hash is included in the runtime fingerprint or release identity (`agent_version_candidate.py:243-289`). | Source the selector from the authorized Eval Dataset catalog (or introduce an explicit release-Dataset contract), bind an immutable Dataset revision/content fingerprint into the candidate and release identity, use tenant-composite integrity where applicable, and add real API + PostgreSQL + browser coverage that does not mock away the boundary. |
| C-02 | high | R1, R4, lifecycle acceptance | `cancelled` is a declared/UI/mocked state but has no real producer. POST `/agents/{id}/evals` resolves, evaluates, stores, and returns synchronously (`src/api/v1/agents.py:557-610`); the evaluator can emit only `passed` or `failed` (`agent_version_candidate.py:355-369`); no cancellation endpoint exists. Repository code accepts `cancelled`, but inserts `queued`, `running`, and the terminal event together only after computation (`agent_repository.py:1781-1783,1895-1930`). Browser lifecycle objects and POST results are synthetic. | Implement an actual cancellable execution lifecycle with durable transitions and authorization, or obtain an explicit requirement change and remove the unsupported cancellation claim/state. Exercise the real API/repository lifecycle, including cancellation races and terminal-state immutability. |
| C-03 | high | R2, idempotent publication | The request key is globally scoped by `(tenant_id, operation, idempotency_key_hash)` (`077_agent_publication_eval.sql:165-199`), but publication serializes on each Agent row before checking that global key (`agent_repository.py:2294-2316`). Two different Agents therefore do not share a lock. A disposable-schema concurrent reproducer with the same tenant/key and different Agents produced exactly `success` plus raw `UniqueViolationError(sqlstate=23505)`. The API maps every raw 23505 to `AGENT_SLUG_CONFLICT` (`src/api/v1/agents.py:303-304`), not the promised stable `AGENT_RELEASE_IDEMPOTENCY_CONFLICT`. Existing tests cover sequential/same-Agent replay and do not expose this race. | Reserve or lock the idempotency-key namespace before work (or scope the key to Agent if that is the contract), handle insert conflicts deterministically, and map release-request conflicts to the stable release error. Add concurrent different-Agent repository and API tests. |
| C-04 | medium | R1, R4, truthful stale state | The UI says a result becomes stale when the resolved Draft **or runtime fingerprints** change (`agents-en-US.json:348`), but list/get mark stale only when Draft revision or spec hash changes (`agent_repository.py:1969-1977,2019-2027`). Model, Skill, MCP/Connector, Knowledge authorization, or runtime-fingerprint drift remains displayed as `passed` until a later publish attempt. The screenshot/test only covers Draft-save staleness. | Recompute or compare current server-owned runtime/resource fingerprints when serving release evidence, expose an explicit stale/blocked reason, and add model/resource drift API and browser cases; otherwise narrow the UI claim truthfully. |
| C-05 | high | R3, current authorization and in-transaction race | Promotion resolves model readiness/access and builds the current candidate before entering the repository transaction (`src/api/v1/agents.py:725-745`; `agent_runtime.py:201-304,426-434`). Inside the transaction `_resolve_version_material` rechecks Skills, MCP/Connectors, and Knowledge, but not model readiness/access (`agent_repository.py:1309-1409`). Rollback similarly builds its model-bearing Snapshot before its transaction (`src/api/v1/agents.py:969-999`) while `_validate_existing_version_resources` omits model reauthorization. A model disable/access change in that window can therefore move the pointer using authorization that is no longer current, despite the receipt claiming `resource_authorization_rechecked: true`. | Revalidate model identity, enabled/provider readiness, and actor access as part of the atomic decision, or compare a locked/versioned model-authorization token inside the transaction. Add deterministic publish and rollback race-injection tests proving pointer/event/request remain unchanged on drift. |

## Requirement Coverage

- **R1 — revision-bound evaluation:** Draft ID/revision/spec and a closed server-built runtime fingerprint are strongly checked; detached client pass/fingerprint/profile inputs are rejected. Coverage is nevertheless incomplete because the selected Dataset flow is wired to the wrong product table, the Dataset itself is not revision/content bound, cancellation is not executable, and runtime drift is not surfaced as stale.
- **R2 — idempotent immutable publication:** immutable evidence/Version triggers, same-request replay, conflicting sequential payloads, and failure rollback are well covered. The cross-Agent global-key race in C-03 means the stable idempotency contract is not complete.
- **R3 — atomic promotion and rollback:** pointer/event/request atomicity, same-channel history, revocation, and never-published-target denial are demonstrated against PostgreSQL. The special never-published Version case passes: the repository requires target history and the database negative leaves pointer/event/request unchanged. Current model authorization is not part of the transaction, so C-05 remains a release-integrity gap.
- **R4 — truthful Studio controls:** roles, blocking findings, prompt-free diff, immutable history, rollback, responsive/dark UI, keyboard/focus, axe, and failure-closed model-unavailable evidence are present. Selected Dataset and cancellation are mocked beyond the backend contract, and resource-fingerprint staleness is overstated.
- **AS-F007:** the implementation covers most publication mechanics, but the five findings above prevent the Oracle step from being honestly marked passing.

## Test and Regression Assessment

The Critic independently ran every required group against the reviewed source:

1. `uv run pytest -q --no-cov tests/api/test_agent_publish_api.py tests/database/test_agent_publication_atomicity.py tests/services/eval/test_agent_publish_gate.py` — exit 0; **20 passed, 0 failed, 0 skipped**, one Starlette deprecation warning.
2. `uv run pytest -q --no-cov tests/services/eval/test_agent_version_candidate.py && make eval-regression-gate && make verify-eval-dev` — exit 0; candidate **11 passed**; golden **16/16** with pass/critical/trajectory rates 1.0; inherited groups **41 + 116 + 35 + 17 passed**, Web lint/type-check passed, 0 skips. Warnings were the known Starlette/OpenAPI warnings and 17 inherited lint warnings.
3. `corepack pnpm@10.33.0 -C web lint && corepack pnpm@10.33.0 -C web type-check && corepack pnpm@10.33.0 -C web i18n:check && corepack pnpm@10.33.0 -C web build && corepack pnpm@10.33.0 -C web exec playwright test e2e/agent-publish.spec.ts --config playwright.opensource.config.ts` — exit 0; lint 0 errors/17 warnings; type-check, i18n, and build passed; Playwright **8 passed, 0 skipped**. The known Node 24 versus requested Node 22 engine warning and 1.395 MB shared-UI chunk warning remain.
4. `make verify-assistant-runtime-dev && make test-isolation` — exit 0; AHR groups **33/77/8/98**, golden gate passed, and isolation **6 passed, 0 skipped** over temporary localhost Gateway `18080` to Assistant `18093` with the explicit stub and external provider keys blank.

These passes support the behavior they execute, but do not falsify C-01 through C-05. In particular, the browser suite intercepts the selected Dataset and evaluation APIs, the candidate/database suites use `dataset_id=None`, lifecycle cancellation has no server test, and idempotency coverage lacks concurrent different-Agent reuse.

All seven source fingerprints recorded in `as-06-publish-atomicity.json` match the current files. The receipt is selected rather than complete: it does not fingerprint `web/src/api/agents.ts`, where C-01 originates. All 12 screenshots have the claimed dimensions and non-constant pixel ranges; direct inspection confirmed rendered desktop/mobile/dark/rollback/version states. This visual evidence cannot establish a real backend Dataset or cancellation path. `git diff --check` exited 0.

## Security, Privacy, and Failure Assessment

Tenant/Agent ACL checks, Owner-only mutations, server-owned profile/candidate facts, prompt-free structured diff, reason redaction, immutable evidence triggers, and failure-closed missing-model behavior are sound within the inspected paths. No Prompt body, Secret, credential, token, connection string, or raw tool output appeared in reviewed durable evidence. The Critic used no live/paid provider and did not read or print credential values.

C-03 is a stable-error/failure-atomicity defect: the losing transaction rolls back, but clients receive a misleading slug error. C-05 is more serious for authorization freshness because the pointer decision can outlive the model authorization used to construct its candidate. C-01 also leaves the database Dataset/run FKs non-composite even though rows are tenant-scoped; application checks reduce current exploitability, but schema-level tenant integrity should be made explicit when correcting the binding.

Before runtime work, all eight `ai-gateway-*` containers were healthy and their Compose ownership labels pointed to `/Users/yang/projects/AI--Platfform`. Sampled memory was approximately 753 MiB, below 3.5 GiB. Temporary processes were stopped; ports `18080` and `18093` were independently confirmed released. The formal Compose services remained healthy.

## Minimal-Change Assessment

The reviewed AS-06 implementation is concentrated in the planned migration, candidate/repository/API, Agent Studio release UI, locales, tests, and evidence. Existing Eval and built-in Assistant boundaries were reused. The working tree contains extensive pre-existing/unrelated changes, so this verdict attributes only the targeted files and matching AS-06 receipts; it is not an approval of the entire dirty checkout. The Critic changed no product source, test, Oracle, loop-state, deployment, secret, or runtime configuration; this verdict is the sole intentional repository write.

## Rollback and Handoff Assessment

Immutable Version history, same-channel historical-target enforcement, revoked/unavailable target denial, atomic pointer/event/request behavior, and existing-session versus new-session pinning are materially evidenced. A manually created never-published Version is correctly rejected as `AGENT_ROLLBACK_TARGET_NOT_HISTORICAL` without state mutation. Rollback cannot be approved for release until current model authorization participates in the atomic decision described in C-05.

`feature-oracle.json` must remain `AS-F007 = failing`; `loop-state.json` must remain on AS-06. This verdict does **not** mutate the Oracle, satisfy a completion gate, or unlock AS-07.

## Whole-Demand Regression Assessment

Inherited Eval and Assistant runtime regressions pass, and the AS-06 focused browser/build suites pass. Terminal same-build whole-demand regression remains an AS-09 responsibility. No aggregate release claim is made here.

## Verdict Rationale

**changes_requested.** The required commands and substantial atomicity/immutability/rollback work pass independently, but green gates are insufficient where the tests mock or omit required seams. The real PostgreSQL Dataset and idempotency reproducers demonstrate two release-contract failures, cancellation has no executable server path, runtime staleness is not reported truthfully, and model authorization is not rechecked atomically. Correct C-01 through C-05 and rerun the same zero-skip gates plus the specified focused real-boundary/race tests before requesting a fresh Critic review.
