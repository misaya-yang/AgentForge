# AS-04 Independent Critic Verdict — Iteration 1

**Phase:** AS-04 — Skills and Knowledge Version Bindings  
**Feature:** AS-F005  
**Critic:** independent fresh-context reviewer  
**Critic Verdict:** `changes_requested`  
**Actor Report:** `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md`  
**Date:** 2026-07-19

## Decision

AS-04 / AS-F005 is not approved on this frozen source. The Skill persistence,
exact-version execution, capability upper bound, normalized binding and
revocation paths have substantial passing evidence, and every required local
command passed when independently rerun. However, the recorded Knowledge
`revision_hash` is not a live-content revision fingerprint: retrieval-effective
segment text can change while the hash remains identical. That is a direct
failure of R3, the AS-F005 Oracle step, the `live-knowledge-provenance` golden
case and a non-waivable acceptance gate.

AS-F005 must remain `failing` and AS-05 must remain locked. The Actor's report,
golden and continuity claims must not be promoted until C-01 is corrected on a
new frozen source and independently reviewed again.

## Inputs Reviewed

- Phase contract:
  `docs/agent-studio-prd/phase-04-skills-and-knowledge-version-bindings.md`.
- Fixed implementation plan:
  `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-plan.md`.
- Oracle item: AS-F005 in `docs/agent-studio-prd/feature-oracle.json`; it was
  still `failing` with no evidence at review time.
- Actor report:
  `docs/agent-studio-prd/reports/as-04-skills-and-knowledge-version-bindings-report.md`.
- Durable golden:
  `reports/agent-studio/as-04-skill-kb-golden.json`.
- Migration and schemas: migrations 037, 071 and 075, including immutable
  Skill versions, composite tenant references, normalized Draft/Version Skill
  and Knowledge bindings, revocation rows and provenance fields.
- Skill implementation: parser, models, artifact repository, scoped registry,
  executor, Gateway API, Assistant bridge, request-local runtime overlay and
  capability resolution.
- Knowledge implementation: Agent save/publish/run resolver, Gateway Snapshot,
  Assistant runtime/trace path, Knowledge Dataset catalog and public segment
  update persistence path.
- Required API, isolation, entrypoint, Skill runtime, Knowledge/runtime stream,
  Ruff and PostgreSQL migration tests.
- Loop state and handoff were read only to confirm the pending independent
  Critic state. This Critic did not edit the Oracle, loop state, handoff, Actor
  report, golden, source, tests, migration or runtime state.

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| AS04-C01 | high, release-blocking | R3; AS-F005 step 3; `live-knowledge-provenance`; acceptance: each run records the current live-content revision | `AgentLoop` hashes Dataset metadata and counts, not retrieval-effective content. The supported segment-edit API can change segment text/vector without changing Dataset `updated_at` or counts, so two different live corpora receive the same `revision_hash`. | Expose and consume an authoritative content-sensitive Dataset revision/fingerprint, fail honestly when it is unavailable, and add a same-count content-edit regression proving the run/trace hash changes. Rerun all affected gates and submit a new frozen report/golden for independent review. |

### AS04-C01 proof

1. `AgentLoop._get_streaming_dataset_context` builds `revision_rows` only from
   Dataset `updated_at`, embedding/configuration fields, `needs_reindex`,
   collection name, and document/segment counts
   (`apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`,
   lines 2723-2755). It does not consume a Knowledge content revision or
   content hash.
2. `DatasetService.list_datasets` returns visible Dataset rows plus aggregate
   counts (`apps/knowledge-service/src/knowledge_service/services/knowledge/dataset_service.py`,
   lines 52-91); it does not add a retrieval-content fingerprint.
3. The supported `PUT /knowledge/{dataset_id}/segments/{segment_id}` route calls
   `DocumentService.update_segment`. That method persists new text and updates
   the vector (`document_service.py`, lines 639-710). Its database operation
   updates only the segment text and the segment's `updated_at`
   (`apps/knowledge-service/src/knowledge_service/persistence/database.py`,
   lines 2532-2567). It does not touch the Dataset row. Editing one existing
   segment also leaves the document and segment counts unchanged.
4. The Critic ran a read-only, provider-free reproduction against the actual
   `_get_streaming_dataset_context` method. A fake catalog held every hashed
   field constant while changing a supplied content revision marker from
   `segment-text-v1` to `segment-text-v2`. Exit was 0 and the exact output was:

   ```text
   {'first': 'd44a24323c5b5893', 'second': 'd44a24323c5b5893', 'hash_equal': True}
   ```

   The ignored marker models the content change performed by the public
   segment-edit path while Dataset metadata/counts remain stable.
5. The required test
   `test_each_run_captures_live_revision_and_explicit_replay_limit` changes only
   the fake Dataset's `updated_at` and asserts that the hash changes
   (`tests/services/assistant/test_agent_knowledge_binding.py`, lines 185-244).
   It never changes retrieval content while holding Dataset metadata/counts
   stable, so its green result does not establish the acceptance claim.

The trace correctly says `content_mode=live_latest` and
`historical_replayable=false`; C-01 is not a replayability overclaim. The defect
is more basic: the value labelled as the current live revision cannot detect a
supported live-content mutation. Consequently, distinct retrieval results can
be attributed to the same provenance hash and drift can be silent.

## Requirement and Oracle Coverage

| Contract item | Critic assessment |
| --- | --- |
| R1 honest Skill persistence and tenant/user isolation | Locally covered. Repository/API reads are scoped, persistence failures are surfaced, server-owned `db://` identities are used, and the required API/isolation/entrypoint command passed. No C-01 impact was found in this path. |
| R2 exact Skill version execution | Locally covered. Full canonical content/hash and exact Version IDs are sealed; the request-local overlay does not enter the global registry or expand the Agent allowlist; disable/revoke paths are checked before execution. The required runtime command passed. |
| R3 authorized Knowledge binding and live provenance | `changes_requested`. Normalized Draft/Version rows and save/publish/run ACL checks have passing tests, but the runtime value is a catalog-metadata hash rather than a content-sensitive live revision. |
| R4 fail-closed revocation and degradation | Partially covered. Skill/Dataset missing, deleted and revoked cases fail closed in the reviewed code/tests. C-01 leaves content drift silently indistinguishable, so the overall Knowledge provenance/degradation claim is not complete. |
| AS-F005 steps 1, 2 and 4 | The exact Skill upload/version, isolation and revoke/unavailable cases have supporting local test evidence. |
| AS-F005 step 3 | Fails C-01. Binding normalization and ACL are covered, but "record the live revision fingerprint" is not. |
| Golden `live-knowledge-provenance` | Its `passed` status is unsupported on this source because its cited test changes Dataset metadata, not content. The overall golden `status=passed` therefore cannot be accepted. |

## Independent Test and Regression Assessment

The Critic reran the four exact Phase commands plus the requested PostgreSQL
migration test on the frozen source. No skip was reported.

| Gate | Independently rerun command | Exact result |
| --- | --- | --- |
| Skill API/isolation | `uv run pytest -q --no-cov tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/security/test_skill_entrypoint_policy.py` | exit 0; 26 collected; `26 passed, 1 warning in 0.42s`; zero skips |
| Skill runtime | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_skill_version_binding.py` | exit 0; 6 collected; `6 passed, 1 warning in 0.48s`; zero skips |
| Knowledge binding/streaming | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_knowledge_binding.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | exit 0; 36 collected; `36 passed, 1 warning in 0.52s`; zero skips |
| Exact AS-04 lint | `uv run ruff check src/api/v1/skills.py packages/ai-gateway-core/src/ai_gateway_core/skills apps/assistant-service/src/assistant_service/core/skills tests/api/test_skills_api.py tests/security/test_skill_tenant_isolation.py tests/services/assistant/test_skill_version_binding.py tests/services/assistant/test_agent_knowledge_binding.py` | exit 0; `All checks passed!` |
| PostgreSQL migration contract | `uv run pytest -q --no-cov tests/database/test_agent_skill_knowledge_migration.py` | exit 0; 3 collected; `3 passed, 1 warning in 0.75s`; zero skips |

The pytest commands each reported the same dependency/TestClient deprecation
warning; no warning was represented as a skip or failure. In total, the four
pytest invocations above passed 71 tests with zero skips. These passing commands
prove only their assertions. C-01 is outside the asserted input variation and
therefore is not contradicted by the green suite.

### Evidence boundary

The Critic personally reproduced only the five command results above and the
provider-free hash reproduction in C-01. The migration test exercised its own
isolated PostgreSQL schema, but the Critic did not independently reproduce the
Actor's claims about applying migration 075 to the repository-owned live
database, authenticated live Skill CRUD, live Agent/Knowledge execution,
Compose ownership, 8/8 service health, approximately 720 MiB stack memory,
hot-source identity, the 60-test adjacent Assistant bundle, AHR aggregate
groups, Docker/open-source checks or any external provider. Those remain
Actor-reported receipts, not Critic-observed facts, and are not used to close
C-01.

No browser evidence was required because AS-04 must not add Studio UI. No real
model, embedding provider or production Knowledge system was called by the
Critic.

## Security, Privacy, and Failure Assessment

- Tenant uploads are parsed as instruction-only artifacts and assigned
  server-owned database identities; the reviewed path does not let Skill
  content create tools beyond the immutable Agent capability upper bound.
- Exact Skill and Dataset authorization/revocation paths are rechecked at the
  reviewed save, publish and run boundaries, with stable unavailable failures.
- Bound-only Dataset filtering prevents an accessible but unbound Dataset name
  from entering Agent context in the covered tests.
- C-01 is a provenance-integrity failure: an authorized content edit is not
  reflected in the purported live revision. It can make audit/eval drift
  analysis wrong without raising an unavailable state.
- The Critic did not print or write secrets, generated `.env` values,
  credentials or provider keys.

## Required Remediation and Re-review

1. Define an authoritative per-Dataset content revision that changes for every
   retrieval-effective document/segment mutation, including create, text
   update, delete, enable/disable and successful reindex changes. A monotonic
   transactional content revision or canonical content fingerprint is
   acceptable; Dataset presentation metadata and counts alone are not.
2. Return that revision through the existing authorized Knowledge catalog or a
   dedicated authorized revision contract, and have Agent runtime hash the
   exact bound Dataset IDs plus those authoritative revisions. If a bound
   Dataset has no authoritative revision, record/fail as unavailable rather
   than silently substituting a metadata approximation.
3. Add an integration/regression case that edits an existing segment's text
   while preserving Dataset `updated_at`, document count and segment count,
   then proves the next run/trace revision changes. Retain the explicit
   `historical_replayable=false` assertion.
4. Rerun all affected exact Phase commands and the migration contract. Amend
   the Actor report and golden only with results from the corrected frozen
   source, then request a fresh independent Critic iteration.

If the Knowledge service cannot supply an authoritative revision contract,
the Phase stop condition "Knowledge permission/revision contract is
unavailable" applies; a metadata-derived fallback cannot satisfy R3.

## Minimal-Change Assessment

The checkout contains extensive inherited modifications across earlier phases
and unrelated work, so a clean phase-only Git diff cannot be reconstructed
from the current working tree. The reviewed AS-04 implementation surfaces are
mostly within the Phase's `likely_edit_paths`, with necessary composition and
streaming integration. No AS-05 Studio UI or publication channel was found in
the AS-04 slice. This Critic preserved all existing changes and wrote only this
canonical verdict.

C-01's correction may require a narrow Knowledge revision-contract change even
though indexing/retrieval algorithm refactoring is out of scope. It must not be
used for a wider Knowledge rewrite.

## Rollback and Handoff Assessment

The reviewed rollback design preserves immutable bindings while separately
disabling Agent-bound Skills/Knowledge, and the relevant local tests pass.
Historical Skill content is not deleted, Skill execution can be killed before
provider use, and current revoke/delete state overrides stored references.
Those are sound rollback properties but do not repair the provenance defect.

The canonical transition remains:

- AS-F005: keep `failing`.
- AS-04: keep active/pending remediation; do not record critic approval.
- AS-05: keep locked.
- Preserve this `changes_requested` verdict as iteration history when a later
  verdict is produced.

## Whole-Demand Regression Assessment

Terminal same-build whole-demand regression belongs to AS-09 and remains
pending. For this earlier Phase, the Critic assessed only the required AS-04
commands and the inherited boundaries visible in the frozen slice. Passing
adjacent or Actor-reported aggregate regressions cannot waive an AS-04
acceptance failure.

## Verdict Rationale

The suite gives credible support for exact instruction-only Skill persistence,
runtime isolation, normalized Knowledge bindings and revocation. Nevertheless,
R3 and AS-F005 explicitly require a current live-content revision/provenance
record on every run. A supported edit can alter retrieved content while keeping
the recorded hash unchanged, and the current test/golden never exercise that
case. Because exact Knowledge provenance is non-waivable and release-blocking,
the only evidence-supported verdict is `changes_requested`.
