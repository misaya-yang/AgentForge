# Repository layout checkpoint — 2026-10-16

The repository and Git root are now
`/Users/misaya.yanghejazfs.com.au/misaya_project/AI-Platform-Rust-Rebuild`.
The former nested `AI--Platfform/` directory has been removed.

## Completed changes

- Moved the complete existing checkout, including Git metadata, into the owner-selected root.
  Before follow-up edits, all 4,666 inventoried source files retained identical bytes, modes
  and symlink targets; HEAD and the index were unchanged by relocation.
- Removed the explicitly unwanted recovery package, verifier, restore instructions and the
  sibling pre-dots Python backup. No replacement source backup was created.
- Kept Rust services in `rust/platform-native/` and Runtime/Worker source in
  `rust/agent-runtime-overlay/`; the root README links their actual Rust entry points.
- Kept the primary Compose file at root and grouped 11 optional overlays in `deploy/compose/`.
  Updated active scripts, Make targets, catalogs, guards, tests and deployment documentation.
  Frozen baselines and dated receipts retain their historical measurements and paths.
- Moved the long operations reference into `docs/deployment/operator-guide.md` and replaced
  contradictory historical instructions with the current owner-authorized Rust workflow.
- Compacted the active program state; its complete previous dated entries remain unchanged in
  `reports/native-current-acceptance-20261016/program-history-before-layout.json`.
- Preserved the existing Compose project through `COMPOSE_PROJECT_NAME` from the selected
  local env. Explicitly excluded `.env.dev` from Git; no tracked local credential files.
- Stopped only this project's services for relocation. Named volumes remain intact.

## Verification

- Whole native workspace/all-target `make native-check` passed after repairing one missing
  `Utc` test import. Existing compiler warnings remain; this is compilation evidence.
- Shell syntax and local links in the rewritten entry documentation passed.
- Source-lock overlay refresh passed and honestly invalidated runnable-image identities.
- Known-secret scanning of the staged tree covered 4,664 files / 4,531 unique blobs against
  53 private credential variants: zero matches; no local env files are staged. There are
  2,102 Rust source files and zero first-party Python source files.
- All 1,234 evaluation fixture files retain exact bytes in the index. Git attributes prevent
  line-ending normalization from invalidating imported vendor checksums on future checkouts.
- Release/development and existing-local Compose rendering passed without starting services.
- Deployment script contracts: 43 passed. Topology/gate contracts: 31 passed.
- Final harness passed with zero errors and five existing historical program warnings.
- Exact commands/log hashes and warning scope are recorded in
  [the machine-readable receipt](repository-layout-20261016.json). No live product acceptance
  is claimed here.

## Continuation after commit/push

Start subsequent work from a clean Git tree. Keep the same `agentforge-dev-7801d4a9a4`
project, volumes and sole existing Buildx builder. Recreate only its containers to update
ownership labels to the new root, then run current source services and real browser journeys.
Source/main DB are epoch22; successor023 remains inactive. The active program state lists the
Assistant start, Knowledge vector/version and Local Node acceptance work. SDK Qwen continuity
and bounded auth evidence remain valid only for the source/process scope in their receipts.

This checkpoint completes repository organization and source delivery. Full enterprise product
acceptance, current-source images and remaining end-to-end workflows are still in progress.
