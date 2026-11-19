# U-PE-01 — Compose image identity

The authoritative state and package contract are in [loop-state.json](loop-state.json).
Scope and acceptance come from [platform upgrade §08](../../../docs/plans/platform-upgrade-2026-10/08-platform-engineering.md#6-work-packages).

## Activation checkpoint — 2026-10-17

- Base: `22d9ce0ff9cba51b130f525e7b1add5cc3853744`, branch
  `codex/platform-upgrade-2026-10`. Existing plan files and `docs/README.md` edits retained.
- Observed: frontend resolves to the Knowledge image; API/worker Knowledge defaults differ;
  root Compose contains source builds; Runtime/Worker defaults name an obsolete source revision.
- Result: rendered Compose identifies separate frontend/Knowledge units, one image for both
  Knowledge roles, and a Runtime/Worker pair selected against the existing lock.
- Preserve: existing `FRONTEND_IMAGE` override, public wire/API semantics, tenant boundaries,
  source lock and artifact validation. Do not introduce a second image-release authority.
- Direct evidence: none yet. Implementation, compilation, execution and live acceptance remain distinct.
- Environment: primary session owns builds; no project containers/volumes or historical builder
  found. Docker reports about 3 GiB RAM; disk has about 55 GiB free. `.env.dev` is absent.
  No runtime mutation or data initialization has occurred.
- Next: add real rendered-config identity validation and negative cases, then repair the config.
  Runtime image labels/digests and live journeys remain separately required under Ph0.

## Direct verification checkpoint

- 68 tests passed (5 identity, 44 deployment-script, 19 CI/harness); zero failed/ignored.
- The regular `make validate-example-config` gate executed 9 actual Docker renders and
  rejected both collision and stale-worker negative cases. CI uses this same entrypoint.
- `make harness-check` passed with zero errors and five existing historical-program warnings.
- Independent review found no blocker/high; its medium matrix-retention gap was repaired.
- Source/config is integrated. Current-image builds, image admission and the Assistant,
  Knowledge and Local Node live journeys remain open. No container or database was changed.
- Source hashes, commands and limits: [receipt](../../../reports/platform-upgrade-20261017/U-PE-01.json).
