# Platform architecture convergence — closeout handoff

**Date:** 2026-08-30
**Branch:** `platform-arch-convergence-2026-08`
**Tested code candidate:** `9aaa9cabcd9284b001b13f1a0b5b2a90f6de0768`
**Base:** `main@336851c107c659342ec79aa3aa298de77b1edc68`
**Observed range:** 127 commits ahead of `main`, 0 behind at the tested candidate

## Outcome

The implementation and core live product paths are closed. ARC-00A/B, ARC-01/01B, ARC-02,
ARC-03, ARC-04, ARC-05 and ARC-07 have direct, integration and applicable live evidence. ARC-06
has its direct topology/admin contracts but not both compact/scale live deployments. ARC-00C,
ARC-02B and ARC-08 cannot be called fully verified because hosted Rust CI/multi-arch evidence,
fresh-machine evidence and the frozen rollback artifacts are unavailable.

No known blocker/high code finding remains in the tested Gateway, Web, Knowledge, database or
Agent execution paths. This is a core candidate, not a fully published release candidate: the
checked-in compatibility manifest intentionally remains `draft`.

## Preserved takeover checkpoints

The original WIP was preserved and completed forward; none of these commits was reset, rebased or
discarded:

| Package | Preserved checkpoint |
| --- | --- |
| ARC-00B | `f327e0f0` |
| ARC-03 | `ec487d80` |
| ARC-02B/00C | `023b06a2` |
| ARC-02 | `9fc5095f` |

## Runtime identity and ownership

- All active `ai-gateway-*` Compose containers are owned by
  `/Users/yang/projects/AI--Platfform`; the deleted `.claude/worktrees/kb-rag-upgrade` checkout is
  no longer the runtime owner.
- Runtime image:
  `ai-gateway-agent-runtime:local-93c54bca3899-c489b7e4d147`,
  `sha256:9d736eb9666166d54215e675525442540384af7c21b1ef81cc63dc613dde7ad6`.
- Capability Worker image:
  `ai-gateway-agent-capability-worker:local-93c54bca3899-c489b7e4d147`,
  `sha256:ac33b36d39462cc2d9db13d8c8f2af0eea15ff6c50054ca31945c8b0d265c3c9`.
- Runtime and Worker were compiled once in Docker multi-stage/BuildKit builders. No host
  Cargo/rustc/rustfmt/clippy/check/test/build was run, and no cache/target cleanup was performed.
- Python/Web closeout fixes were applied to the current containers with `make hot-update`; all
  services are healthy. The app image labels predate those hot-updated files, so the draft
  compatibility manifest does not claim an immutable all-service candidate image set.

## Executed verification

- `make harness-check`, boundary gates, offline/live OpenAPI, hygiene, LOC no-growth, CI selector
  negative tests, source-contract and singleton guards: exit 0.
- Gateway unit gate: 2107 passed, 3 deselected, 0 failed.
- Knowledge unit gate: 1850 passed, 1 configured skip, 0 failed; the live Knowledge journeys cover
  the changed path.
- Database suite: 447 passed, 2 explicitly isolated-DSN skips, 0 failed. Migration gate: 144 passed.
- Web: type-check, lint, 121 Node tests, i18n and production build passed. The host used Node 24,
  while package metadata declares Node 22; the commands emitted an engine warning but no failure.
- Rust source/wire/supply-chain contract: 10 Python-side contract tests passed and both Docker
  images started healthy. Hosted Rust fmt/check/changed-crate tests were **not run**.
- Final `make status`, `make validate` and `make live-openapi-contract-gate`: exit 0; one local
  bootstrap-password warning remains in config validation.

## Database closure

- Single authority is adopted at baseline `2026_08_post_kb_v1`; there are no pending legacy files
  or post-baseline epoch changes.
- The duplicate 016/030/031 histories were reconciled from structural/checksum evidence, not
  guessed numeric order. Legacy split-schema rows were merged losslessly before cutover.
- Current verification reports all four fingerprints matching and all 251 structural/ACL/location
  checks matching.
- Fresh, upgrade, idempotency, checksum, locking/concurrency, crash transaction, role positive and
  negative permissions, grants and rollback-class paths ran against authorized PostgreSQL test
  targets. Shared volumes were not pruned and `down -v` was never used.

## Real UI and provider closure

Using the persistent ignored local admin identity (no new account), the compiled Web UI and current
Compose stack completed:

- login, dashboard, Assistant chat and session history;
- real DashScope/Qwen streaming and autonomous Knowledge search over a real PDF;
- tool activity ordering, approval allow/reject, cancellation and follow-up recovery;
- Knowledge upload, ingestion success, retrieval, and exact failure reporting for a textless PDF;
- Agent Studio catalog and Preview, with a persisted successful trace visible in the trace page;
- admin architecture status with live Knowledge Worker and Qdrant dependency health.

The two intentionally failed upload documents were deleted through the UI. The completed real PDF
remains as local acceptance data.

## Explicitly not PASS

1. **Hosted Rust CI / multi-arch:** no pushed SHA or remote builder receipt exists. Local Docker
   arm64 images prove runtime behavior but do not substitute for hosted fmt/check/tests or an amd64
   artifact.
2. **Fresh machine:** no separate fresh-machine/registry pull was run. The user directed this
   closeout to keep using the current local Compose stack rather than create another isolated stack.
3. **Current → frozen → current:** `make agent-runtime-rollback-rehearsal` stopped safely before
   changing containers because the frozen Runtime, Assistant and Gateway image IDs recorded by the
   rollback bundle are no longer present and have no registry reference. Rebuilding Rust is outside
   the active authorization.
4. **Candidate compatibility manifest:** draft validation passes, but it is not promoted to
   `release_candidate` without the missing immutable image and zero-skip receipt set above.
5. **Optional local toolchains / Docker registry:** SDK SSE passed for Python and CLI; Java and
   Dart were not run because Maven/Dart are absent. The Agent Studio aggregate reached 37/40 before
   repair: two failures were the same stale Knowledge route fixture (fixed; 48/48 rerun passed), and
   the remaining built-Nginx smoke was blocked resolving the public Dockerfile frontend image.

These are release-evidence blockers, not known failures in the current running core product. They
must remain visible in `loop-state.json`, `work-packages.yml` and the package receipts.

## Next authorized action

Provide/restore the frozen image bundle and a hosted Rust CI or multi-arch builder, then run the
fresh-machine receipt, current→frozen→current rehearsal and candidate compatibility manifest. Do
not push, merge `main`, publish images or deploy production without separate user authorization.
