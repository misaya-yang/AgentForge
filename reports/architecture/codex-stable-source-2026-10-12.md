# Codex stable source-only update: 2026-10-12

## Source unit and measured evidence

- Official stable release: `rust-v0.162.1`.
- Immutable upstream: `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`.
- Upstream tree: `572cdecd45cd985f0ad80b63d8b86ffcb345f47a`.
- Complete overlay: 130 files, SHA-256 `d2eb4de0c28867e5ce5f83de277477bb9672b2599d82ae26270e07086cabe9ad`.
- Resolved lock: `a99c2af69c4272c5a527dc55e1e8b6f4717e783c8f224da0786cc97e90a81341`.
- Compiled schema: 1,060 files, 4,037,337 bytes, SHA-256 `0e8f4bcb6abd17e538e875e0dbe77dbc1e521015bab7f44dfebae4d6e072d49f`.
- Genuine unfiltered locked metadata: 1,633 source SBOM components; Worker closure: 621 components.

The exact composed source was checked across all 9,111 files. Runtime and Worker
all-target checks passed under Rust 1.95.0. Thirteen shared capability-contract
unit tests passed. A later standalone protocol example actually compiled and
exported schema. Its first attempt stopped on the disk guard; the preserved retry
succeeded. The first offline unfiltered metadata export lacked a non-Linux cached
dependency; after the locked dependency cache was populated, a fresh offline
unfiltered export succeeded. Failure receipts are not represented as successes.

The receipt's Runtime/Worker compilation flags remain false because no linked
Runtime/Worker executable was produced. `cargo check` success is recorded
separately and does not justify executable/image claims. The source receipt is
host-generated, with no inherited Docker provenance.

## Exact schema comparison

The old immutable tree's checked-in JSON and TypeScript schema files reconstruct
the previous release bundle exactly: 1,054 files, 4,017,931 bytes, SHA-256
`0bbd1819a6f910bd40f4143022897a502156d26011736e9f0455476b6d2492ab`.
The actual newly compiled bundle changes 90 paths: 14 additions, 8 removals and
68 modifications. This is an exact release-bundle comparison, not an estimate
from Rust declarations. The machine inventory is retained alongside this report.

Static hosted-adapter review:

- Turn start/turn records add parent/root turn lineage. The target port preserves
  `WithTurnExtensionData` and native root attribution alongside signed platform
  metadata; all-target checks validate construction against the new types.
- Assistant `MessagePhase` gains `partial_answer`; native history retains the
  full target wire format. The public V1 text projector uses `AgentMessageDelta`
  text and does not dispatch on phase. Live provider and full old-client journeys remain unrun.
- Sub-agent activity gains model/reasoning metadata. Native target records retain
  those fields; existing public collaboration projection is not a promise that
  every newly added native diagnostic is exposed in V1.
- Native item pagination adds anchor/cursor variants; goal mutation gains origin;
  attachment-owner listing is new. The private hosted API does not expose the
  complete upstream ClientRequest surface. Existing public pagination and
  attachments are not replaced by these native request types.
- `thread/prediction/updated` is experimental. Static search finds protocol/media
  routing and TUI references, but no App Server producer at this target. It has
  no hosted V1 projection: the existing projector wildcard does not map it.
  Treat it as an unexposed diagnostic, not an accepted new hosted feature;
  explicit ignore/projection tests remain part of runnable acceptance.
- Native plugin presentation types change/remove fields; MCP OAuth/account,
  browser-extension requirements, skill metadata and provider capability fields
  also change. Native plugins/Apps/Guardian and the general native account/browser
  surfaces remain outside this hosted managed profile. Platform Worker plugin
  activation is a separate authenticated path and is retained.

Focused offline consumer review additionally passed 11 actual fixtures (3 Gateway
Python tests and 8 Node CLI/Web tests), including partial-answer provider/history
projection, durable Web replay and added sub-agent model/reasoning metadata. Schema
checks preserve all 104 existing request methods and all 83 existing notification
methods/envelopes; one request and one notification are added. Five old/new
request fixture pairs and six phase-schema cases passed. Changed request objects
add no required fields. Removed native plugin/provider fields have no examined
Gateway/CLI/Web callers. No consumer adaptation is warranted by these results.

These focused tests describe the existing boundaries; they do not claim a complete
old-client, generated-SDK or live-event compatibility test. No public OpenAPI/SDK
artifact is regenerated merely because the internal App Server schema changed.

## Admission and remaining acceptance

The state is `local_source_locked`; every existing source-lock OCI artifact has
startup disabled and null image reference/digest. The source-only CLI validation
passes, while explicit Runtime/Worker/App Server runnable admission fails by
design. The current `make agent-runtime-source-contract` also requires a runnable
Runtime and must not be reported as passed in this state.

The exact checked overlay includes the candidate COMPATIBILITY.md byte-for-byte;
its older verification wording is superseded only for the bounded compiler/schema
checks described here, not rewritten to invalidate source evidence. The frozen Runtime/Worker source requires at least the epoch-15 capability
authority schema and grants. The current paired Gateway/universal-SDK rollout
requires epoch 17 (including the intervening epoch-16 lineage changes), as owned
by `docs/architecture/sdk-capability-authority.md`; epoch 15 is a source minimum,
not the deployment target for the current platform. Runtime/Worker
library test execution, executable linking, images, cross-platform behavior,
provider journeys, namespace/approval/cancel/recovery behavior and full old-client
acceptance are not established by these receipts. Same-database rollback to the
old pin is not validated; preserve the documented stop-write/backup-and-restore
boundary.
