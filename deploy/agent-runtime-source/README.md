# Agent Runtime source release lock

This directory is the main repository's immutable reference to the separately
maintained upstream Agent Runtime source. Runtime deployment must never follow a
branch, mutable tag, local checkout, or unverified App Server schema.

Files:

- `lock.json` pins source, schema, SBOM, license, and separate App Server,
  Agent Runtime and capability worker OCI identities. One artifact cannot satisfy another's gate.
- `source-receipt.json` is generated from one clean fork revision and its
  source-built App Server schema bundle.
- `sbom.cdx.json` is the deterministic CycloneDX inventory for that revision.
- `NOTICE.md` preserves upstream attribution and the exact audited hashes.
- `Dockerfile` builds the Phase 0 source-pinned App Server protocol probe.
- `Dockerfile.runtime` builds the private Rust HTTP/SSE Agent Runtime after the
  controlled fork revision and release receipt are updated together.

Generate source evidence from the independent fork:

```bash
python3 scripts/harness/agent_runtime_supply_chain.py generate \
  --fork /absolute/path/to/agent-runtime-source \
  --schema-dir /absolute/path/to/source-built-schemas \
  --receipt deploy/agent-runtime-source/source-receipt.json \
  --sbom deploy/agent-runtime-source/sbom.cdx.json
```

Then update `lock.json` with the generated hashes and immutable image digest.
`make agent-runtime-source-contract` is the fail-closed admission gate. A source-only
receipt is useful during development but does not permit the Agent Runtime
to start.

Refresh and build a new local release unit only after the controlled fork is
clean and committed:

```bash
python3 scripts/harness/agent_runtime_supply_chain.py refresh-source-lock \
  --repo-root . --lock deploy/agent-runtime-source/lock.json
AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-source-build-local
AI_PLATFORM_AGENT_RUNTIME_SOURCE=/absolute/fork make agent-runtime-build-local
AI_PLATFORM_AGENT_RUNTIME_IMAGE=ai-gateway-agent-runtime:local-<sha> \
  make agent-runtime-smoke
make agent-runtime-contract
```

Refreshing source identity invalidates every prior image. The existing
`scripts/rust/build-update.sh --artifact all` builds Runtime and capability worker
serially, then checks both selected images against the source lock. App Server
remains a separate protocol-probe artifact.

`make quickstart-build` and `scripts/new/deploy.sh --build` use both existing Rust
builders before Compose builds the Python/Web images. A prebuilt Runtime never
substitutes for a missing Worker. Deployment verifies the actual selected image
IDs, platform, source/schema/binary labels and the Worker's full overlay hash
before stopping or starting application services:

```bash
python3 scripts/harness/agent_runtime_supply_chain.py verify-local-images \
  --repo-root . --lock deploy/agent-runtime-source/lock.json \
  --runtime-image <runtime-tag-or-digest> --worker-image <worker-tag-or-digest>
```

Source hot-update uses the same pair check and compares container image IDs,
so replacing an image behind the same tag still recreates the Runtime/Worker
pair. Existing stopped Python containers can receive Gateway/Knowledge source
and both shared packages without starting the old application: site-packages is
queried by an isolated `python` entrypoint from that container's immutable image.
The frontend entrypoint is copied with execute permission, then runs on the final
restart. `--no-restart` copies files without starting services or probing app health.
Hot-update does not rewrite container environment values; deployment requires any
explicit kernel revision to match the selected source unit before building or
changing application services.

Source identity checks do not claim that native execution, Docker health,
provider calls, rollback, publication or another platform has been verified.

When the platform overlay or capability migration changes, refresh its derived
identity and Worker dependency SBOM from the clean, controlled composed source
workspace. This command only reads that workspace; it does not modify it:

```bash
python3 scripts/harness/agent_runtime_supply_chain.py refresh-overlay \
  --repo-root . \
  --lock deploy/agent-runtime-source/lock.json \
  --cargo-workspace /absolute/path/to/composed/codex-rs
```

The source synchronization policy and kernel decision are recorded in
`docs/architecture/ADR-006-agent-runtime-single-kernel.md`; model, state, and
capability boundaries are in `ADR-007-agent-runtime-data-boundaries.md`.

## Non-runnable upstream source candidates

Before a release-input refresh, an external source-candidate package can retain
a complete reviewed overlay and its actual Cargo-resolved lock without advancing
the live pin or admitting an image:

```bash
python3 scripts/harness/agent_runtime_source_candidate.py \
  --package /absolute/path/to/source-candidate-package \
  --upstream /absolute/path/to/existing-clean-upstream \
  --repo-root . \
  --composed-source /absolute/path/to/composed-source
```

This read-only, offline gate verifies target Git objects, payload and evidence
hashes, exact live preimages, and the complete composed source when supplied.
It rejects changes to its source-only admission flags and never writes a source
lock or fetches dependencies. Success is source validation, not release admission.
The supplied package uses `ai-platform/runtime-source-unit/v1`, distinct from the
release receipt and v2 overlay manifest.

Source-only promotion to `local_source_locked` does not require OCI images, but
it does require actual compiled schema and matching source/Worker SBOM, receipt,
license/notice and overlay provenance. Generate those from the exact source unit
before refreshing the live lock. Keep all artifact startup flags false until
each corresponding image is built and verified. Never mix target-only overlay
paths or a target lockfile with the old upstream pin.

### Generate evidence for a verified composed candidate

The separate offline `generate-composed` command retains clean immutable upstream
provenance while deriving dependency identities from the exact reviewed overlay
and resolved composed lock. It does not relax `generate`'s clean-fork requirement
or permit a clean upstream-only dependency graph to stand in for platform crates.

```bash
python3 scripts/harness/agent_runtime_supply_chain.py generate-composed \
  --package /absolute/path/to/source-candidate-package \
  --upstream /absolute/path/to/clean-immutable-upstream \
  --composed /absolute/path/to/composed-source \
  --repo-root . \
  --export-evidence /absolute/path/to/export/export.json \
  --schema-dir /absolute/path/to/generated-schema \
  --output /absolute/path/to/new-source-evidence
```

All inputs are read-only. The output must be a new external directory outside
source, repository, candidate and export inputs. It contains a generated source
receipt, source SBOM, Worker SBOM and v2 overlay manifest. It contains no source
lock and grants no image/startup admission. The clean upstream checkout must have
HEAD equal to the target, the expected tree and official origin. The existing
candidate gate verifies the exact composed inventory, modes, target blobs,
overlay payload, evidence and live preimages before any output is written.

An export is admissible only after the compiled protocol example actually exits
successfully and generates a nonempty schema bundle. Export metadata from that
same composed workspace with `cargo +<pinned-version> metadata --locked --offline
--format-version 1`, without platform filtering; retain the exact lock and actual
Cargo/rustc version outputs. Do not infer generation from `cargo check`, source
schema files, a killed build, an empty directory, or a previous release's bundle.
The guarded runner's successful command receipt must include its actual `cwd`;
the metadata command receipt also binds its captured stdout via `stdout_sha256`.

The export manifest uses `ai-platform/composed-source-export/v1` and contains:

- `source_unit_sha256_before` and `source_unit_sha256_after`: the identical
  verified source-unit manifest digest before and after exporting
- `target_commit`, `resolved_lock_sha256`, and `schema_bundle` (the actual
  `bundle_identity` result: SHA-256, file count and total bytes)
- `files`: named `cargo_metadata`, `cargo_lock`, `cargo_version`, `rustc_version`,
  `schema_command_receipt`, and `metadata_command_receipt`; each has a relative
  `path` below the export manifest directory and the actual file's `sha256`

Command receipts retain their exact command, working directory, exit code and
stop reason. Schema generation must use the existing `codex-app-server-protocol`
example `ai_platform_export_schema` with the exact output directory, pinned
Rust, `--locked` and `--offline`. Metadata command flags are likewise validated.
Version files, schema, lock, metadata and both receipts are hash-bound; path
packages must belong to the verified composed tree and each metadata package
must occur in the resolved lock. An export manifest records observed work; it
must never be filled in to make unperformed or unsuccessful commands appear valid.

The generated receipt explicitly records that the protocol schema was compiled,
while Runtime/Worker compilation and Docker image verification remain false.
After source evidence review, update notice/owning docs and the complete release
input unit together, then use `refresh-source-lock` to invalidate prior OCI
identities. Successful generation alone does not authorize live pin refresh,
service startup, publication, or deployment.


## Stable candidate source lock: 2026-10-12

The source-only update selects official stable `rust-v0.162.1`, immutable target
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`, plus the complete 130-file reviewed
platform overlay and actual resolved Cargo.lock. Its genuinely compiled protocol
schema contains 1,060 files. Source and Worker SBOMs were generated from the
unfiltered locked composed graph. The receipt records host protocol generation;
it does not claim a Docker build or linked Runtime/Worker executable.

The state is `local_source_locked`: all three OCI artifact startup flags are
false and previous image references/digests are cleared. Check source identity
without weakening the existing runnable gates:

```bash
python3 scripts/harness/agent_runtime_supply_chain.py validate \
  --repo-root . --lock deploy/agent-runtime-source/lock.json
```

`make agent-runtime-source-contract` currently requests a runnable Runtime
artifact, as does `make agent-runtime-contract`; both must remain blocked until
matching images exist. A passing source-only CLI check is not a pass for either
Make target. The measured schema comparison and remaining acceptance scope are
recorded in [the source-only update report](../../reports/architecture/codex-stable-source-2026-10-12.md).
