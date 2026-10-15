# Phase 02 - Docker-built candidate Runtime and Worker images start in Compose and the Gateway reaches the new Runtime.

- PHASE_ID: ARU-02
- FEATURE_ID: ARU-F003
- DEPENDS_ON: ARU-01

## Outcome

Docker multi-stage builders produce source-identified Runtime and Worker images, and this checkout's Compose project boots them for a real Gateway request.

## Scope

In:

- `scripts/rust/build-update.sh`, existing Runtime/Worker Dockerfiles, image locks, and candidate Compose overrides.
- Candidate service health and Gateway-to-Runtime connectivity.

Out:

- Host Rust compilation, image publication, production deployment, cache pruning, or provider/UI acceptance.

## Done when

- [x] Runtime and Worker images were compiled only inside Docker with matching source/image labels and no repository `target` output.
- [x] Candidate containers are owned by this checkout, healthy, and reachable from Gateway.
- [x] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Docker build | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/Users/yang/projects/opensource-harness/codex-harness scripts/rust/build-update.sh --artifact all` | Serialized Docker builders compile and lock both candidate images. |
| Candidate health | `make status` | This checkout's Gateway, Runtime, Worker, and dependencies are healthy. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop before mutating containers if Compose ownership labels do not point to this checkout.
