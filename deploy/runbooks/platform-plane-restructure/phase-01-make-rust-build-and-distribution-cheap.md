# Phase 01 - Make Rust build and distribution cheap

- PHASE_ID: PPR-01
- FEATURE_ID: PPR-F002
- DEPENDS_ON: PPR-00

## Outcome

One authoritative command turns a scoped Rust source change into a healthy, service-sized, supply-chain-recorded container without rebuilding unrelated artifacts.

## Scope

In:

- Replace the agent-runtime toolchain runtime stage with an approved minimal non-root base that still provides required certificates and runtime libraries.
- Make `deploy/agent-runtime-source/lock.json` the only editable image identity; derive environment and Compose defaults from it.
- Derive each artifact identity from its true source closure so worker-only edits do not change runtime identity.
- Measure cold and warm edit-to-healthy-container time, image size, cache behavior and reproducibility.

Out:

- Binary behavior changes, supply-chain semantic weakening, or unrelated Cargo refactors.
- Keeping duplicate editable pins as fallbacks.

## Done when

- [ ] Agent-runtime image is at most 150 MB, runs non-root, has the required trust store and passes health and live product checks.
- [ ] One command performs build, SBOM/receipt generation, lock update and derived-pin refresh.
- [ ] No independently editable image tag remains outside `lock.json`.
- [ ] A capability-worker-only fixture change leaves agent-runtime identity unchanged without modifying production sources for the test.
- [ ] Warm one-line edit to healthy container is at most 15 minutes; cold and warm results are both recorded.
- [ ] Supply-chain, security review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Supply-chain contract | `make agent-runtime-source-contract` | Lock, SBOM, receipt and sources agree |
| Derived pins | Repository test scans for independently editable local image tags | One authority |
| Artifact closure | Controlled source-closure fixture comparison | Worker edit does not invalidate runtime |
| Runtime image | Image inspection, healthcheck and non-root identity | Small image remains operational and hardened |
| Round trip | Timed canonical build/update command | Delivery friction meets the gate |

## Stop or confirm

- Read `docs/harness/runtime-and-secrets.md` before Docker work and confirm Compose ownership.
- Ask before changing the runtime base-image family or rebuilding the active runtime.
- Stop rather than add a fat compatibility layer if the minimal base lacks an explicitly required runtime dependency.
- Required review: independent supply-chain and container-security review.
