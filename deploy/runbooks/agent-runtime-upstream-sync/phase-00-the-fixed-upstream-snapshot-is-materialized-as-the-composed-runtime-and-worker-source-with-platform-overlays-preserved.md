# Phase 00 - The fixed upstream snapshot is materialized as the composed Runtime and Worker source with platform overlays preserved.

- PHASE_ID: ARU-00
- FEATURE_ID: ARU-F001
- DEPENDS_ON: none

## Outcome

The composed Docker source expands the complete selected upstream tree at `94cbbddafc` and applies the minimal platform overlay with aligned source receipts and no unresolved merge state.

## Scope

In:

- `/Users/yang/projects/opensource-harness/codex-harness@94cbbddafc` as a read-only source.
- `rust/agent-runtime-overlay/**` and `deploy/agent-runtime-source/{lock.json,source-receipt.json,*sbom*.json}`.
- Existing Docker-context materialization and identity scripts.

Out:

- Runtime behavior tests, Compose startup, provider calls, UI acceptance, or upstream feature exposure.

## Done when

- [x] Selected upstream, composed Cargo workspace, overlay manifest, receipt, SBOM, and source lock identify the same source unit.
- [x] No merge markers remain and all Runtime/Worker build-closure files come from the complete selected upstream snapshot plus the declared overlay.
- [x] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Upstream exists | `git -C /Users/yang/projects/opensource-harness/codex-harness cat-file -e 94cbbddafc1776d5e377bca1b05932c697e82238^{commit}` | The selected source object is locally available. |
| Source identity | `make agent-runtime-source-contract` | Receipt, SBOM, overlay, Cargo lock, and image/source identity agree. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Block if composed-source metadata would require Cargo/Rust tooling on the host; adapt the existing flow to Docker instead.
