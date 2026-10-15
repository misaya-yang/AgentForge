# Phase 01 - Runtime shell, append-only ThreadStore, and V1 event projection

- PHASE_ID: CHR-01
- FEATURE_ID: CHR-F002
- DEPENDS_ON: CHR-00

## Outcome

A Agent-backed candidate service can persist and resume ordered Thread/Turn/Item state while producing the existing V1 stream contract.

## Scope

In:

- Candidate Runtime shell, `PostgresThreadStore`, append-only migrations, runtime assignment, and V1 projector.

Out:

- Real provider calls, business tools, public V2 API, or deletion of Python control.

## Done when

- [x] Create/append/flush/load/resume/archive and cursor reads survive process restart.
- [x] Concurrency, idempotency, tenant isolation, and V1 projection contract tests pass.
- [x] Existing affected behavior still passes its smallest relevant regression check.
- [x] The clean fork revision is source-locked, both OCI artifacts are rebuilt,
  and the isolated Runtime image smoke passes.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| ThreadStore contract | `make agent-thread-store-contract` | A second Runtime instance resumes gap-free, non-duplicated Items and V1 projection. |

## Checkpoint 1 (2026-08-21)

Completed:

- Narrow Agent host injection seams for ThreadStore, typed extensions, and a
  host-reserved durable root identity; default upstream behavior is unchanged.
- Rust `ai-platform-agent-runtime` crate using the real in-process App Server,
  with no second model/tool loop.
- Additive PostgreSQL root/member/projection/snapshot/item model and a Rust
  `PostgresThreadStore` covering create, append, load, metadata, archive, and
  tombstone semantics.
- Live `thread/start` round trip through an isolated PostgreSQL schema.

Still required before this phase is done:

- Private HTTP/SSE service boundary and `assistant-turn-contract/v1` projector.
- Durable cursor replay and a second-process resume/archive test.
- Stable session runtime assignment in Gateway and candidate container wiring.

## Checkpoint 2 (2026-08-21)

Completed:

- Private Rust HTTP lifecycle service with liveness/readiness, internal-token
  authentication, tenant/user/session scope verification, create, resume,
  archive, unarchive, bounded live events, and durable SSE cursor replay.
- V1 projector persists visible reasoning summaries, text, tool call/result
  pairs, and terminal events while explicitly discarding raw hidden reasoning.
- A second Agent Runtime instance resumes the same PostgreSQL Thread, then
  archives and unarchives it. The contract exposed and fixed rollout/V1 event
  log mixing and the upstream assumption that every remote Thread has a local
  rollout path.
- Immutable, prompt-agnostic session runtime assignments in Gateway. A Agent
  assignment cannot silently fall through to the Python control loop before
  Phase 2 Turn routing exists.
- Optional `agent-runtime` Compose profile and dedicated Rust Runtime image
  definition; the default stack still starts only Python control.

Still required before this phase is done:

- Commit the controlled fork, regenerate the source receipt/SBOM/schema lock,
  build the pinned Runtime OCI, and start the optional Compose profile.

## Checkpoint 3 (2026-08-21)

Completed:

- Upgraded the supply-chain lock to distinguish the Phase 0 App Server probe
  from the Phase 1 Agent Runtime. `agent_runtime` now fails admission until its
  own binary, protocol, digest, platform, and source labels are locked.
- Added atomic source-lock refresh and local-image recording. A new source
  receipt invalidates every old image, so mixed-revision release units cannot
  pass accidentally.
- Added clean-fork Runtime image construction and an isolated Docker smoke that
  uses an internal-only network, ephemeral PostgreSQL, isolated `AI_PLATFORM_AGENT_HOME`,
  and a second Runtime process to verify durable resume.
- Bazel module update completed; the Python 3.11 lock check passed with no
  `MODULE.bazel.lock` diff. System Python 3.9 remains too old for the wrapper.

Verified:

- `make agent-thread-store-contract`: Python 4 passed, Rust 6 passed, live
  two-process PostgreSQL/HTTP/SSE contract 1 passed.
- Gateway/session/supply-chain cohort: 29 passed; changed-file Ruff passed.
- Harness, config, Compose profile, Dockerfile check, App Server artifact lock,
  and strict program validation passed.
- Runtime-unlocked, dirty-fork-build, and wrong-artifact-smoke negative gates
  all rejected locally before mutation.

Boundary:

- Agent Turn assignments remain fail-closed until Phase 2 implements and
  verifies the private model-only data plane.

## Checkpoint 4 (2026-08-21)

Completed:

- Committed the controlled fork at `44d926ab7c9efe0f2ff42c099f11c18d81e8197a`
  and regenerated the 292-file App Server schema bundle, CycloneDX SBOM, source
  receipt, and multi-artifact lock from that exact clean revision.
- Rebuilt and independently locked `codex-app-server` and
  `ai-platform-agent-runtime` images. The Runtime build uses `lld` from a
  configurable Aliyun HTTPS mirror so the complete embedded kernel links under
  the 4 GiB Docker budget.
- Added a fail-closed container bootstrap marker before Agent `arg0` creates its
  own helper directory. Empty isolated homes initialize; non-empty foreign
  homes, symlinks, host credentials, configuration, and `AGENTS.md` remain
  rejected.
- Ran the isolated Docker lifecycle against ephemeral PostgreSQL: create a
  Thread, terminate the first Runtime container, start a second process, and
  resume the exact same Thread and root membership.

Verified:

- `make agent-runtime-contract`: both artifact identities and digests passed.
- `AI_PLATFORM_AGENT_RUNTIME_IMAGE=ai-gateway-agent-runtime:local-44d926ab7c9e make agent-runtime-smoke`:
  passed; observed cgroup memory was 36,573,184 bytes on first start and
  14,020,608 bytes after restart.
- Runtime binary size is 215 MiB and the shared-base local image is about
  584 MiB; the earlier 4 GiB failure was a GNU linker build peak, not runtime
  RSS.

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Do not apply migrations to shared data or route real user traffic in this phase.
