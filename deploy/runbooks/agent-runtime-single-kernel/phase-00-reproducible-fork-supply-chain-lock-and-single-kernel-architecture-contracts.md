# Phase 00 - Reproducible fork, supply-chain lock, and single-kernel architecture contracts

- PHASE_ID: CHR-00
- FEATURE_ID: CHR-F001
- DEPENDS_ON: none

## Outcome

A maintainer can prove the exact Agent source, fork revision, schema bundle,
toolchain, license, and image identity that a candidate runtime is allowed to run.

## Scope

In:

- The independent local fork and upstream sync policy.
- Single-kernel and model-plane/storage ADRs.
- `deploy/agent-runtime-source/` lock, NOTICE, source receipt, and validation gate.
- Harness docs and CI-facing canonical command registration.

Out:

- Runtime routing, database schema, provider traffic, tools, Web, Docker startup, or production publication.
- Creating or pushing a remote fork or OCI image without explicit authorization.

## Done when

- [ ] The local fork is pinned to one upstream commit on `ai-platform/main` and has no platform code scattered through upstream core.
- [ ] The lock validator fails closed on missing hashes, mutable image tags, schema drift, or a missing Apache notice.
- [ ] The source-built App Server schema receipt matches the lock and records the pinned Rust toolchain.
- [ ] Both ADRs, runtime topology, command catalog, and program state agree on one target kernel.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Supply-chain lock | `make agent-runtime-source-contract` | Exact immutable source/schema/license/build identity is present and internally consistent. |
| Program contract | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py deploy/runbooks/agent-runtime-single-kernel --strict` | No placeholder, phase, dependency, or handoff drift. |
| Repository harness | `make harness-check` | New architecture docs and command remain reachable and mechanically valid. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- A remote fork URL, pushed revision, registry image digest, or production rollout requires owner authorization.
