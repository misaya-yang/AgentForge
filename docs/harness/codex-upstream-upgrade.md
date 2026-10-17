# Codex upstream source upgrades

AgentForge embeds the Rust Codex execution kernel through
`rust/agent-runtime-overlay/kernel-rs/`. The reference repository named
`codex-harness` is the OpenAI Codex CLI source tree. It supplies no AgentForge
`AGENTS.md`, `harness.yml`, or repository-workflow migration template. Its CLI
examples, Python SDK and development setup do not configure platform services.

This procedure extends the existing [source-release workflow](../../deploy/agent-runtime-source/README.md).
Execution status remains in
[`native-enterprise-upgrade/loop-state.json`](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json).
Source migration does not create another program or grant runtime, build,
provider, database or publication permissions.

## Identity and ownership

The 2026-10-17 adaptation targets immutable upstream commit
`ce0323f2f2d3b25e82806cd555237da3db4e1897`, tree
`4e648d23f0d36f589e1945ddd3daca53ab3dd8bc`, from the owner's local reference.
The previous source pin was `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`.
The new source still declares Rust 1.95.0. “Latest” identifies that inspected
snapshot, not a continuing branch-following deployment policy.

The integration writer owns root Cargo manifests/locks, overlay inventory,
source receipt/SBOM, release lock, schema generation and serialized builds.
Workers adapt assigned source paths against clean old and new upstream trees.
Preserve current project changes while composing the candidate. Never reset the
reference repository, overwrite another worker's service files, or refresh an
identity merely to make a mismatch disappear.

## Source composition

1. Record the old source lock and exact new commit/tree. Check both upstream
   checkouts are clean and the old checkout contains every declared
   `upstream_blobs` preimage. A composed workspace is not an upstream preimage.
2. Three-way replay each overlay replacement: current project file, old pinned
   upstream blob, new upstream blob. Resolve conflicts by preserving platform
   behavior and the new upstream contract. Textual merge success still needs
   compilation and behavioral verification.
3. Classify project-added files as absent upstream only after checking the new
   tree. Detect file/directory collisions. Rebase the workspace manifest
   separately; resolve the new Cargo lock in the controlled workspace under the
   integration writer. Do not carry forward stale dependency resolution.
4. Assemble immutable upstream plus adapted overlay in external scratch. Retain
   preimage hashes, modes, proposed file hashes and real commands in source-unit
   evidence consumed by the native tools.
5. Generate App Server schemas with the compiled protocol library and export
   resolved Cargo metadata from that same workspace. Use
   `source-lock generate-composed` with the real exports; its argument contract
   lives in `rust/platform-native/crates/platform-tools/src/source_refresh.rs`.
6. Integrate the coordinated overlay manifest, receipt/SBOM and source lock.
   Source refresh invalidates old image admission. Build and probe each required
   artifact before recording its new image identity.

The existing native tools remain the implementation authority:

```sh
platform-tools source-candidate verify \
  --repo-root . --package "$SOURCE_PACKAGE" --upstream "$UPSTREAM"
platform-tools source-candidate assemble \
  --repo-root . --package "$SOURCE_PACKAGE" --upstream "$UPSTREAM" \
  --output "$COMPOSED_SOURCE"
platform-tools source-lock verify-overlay \
  --repo-root . --source "$UPSTREAM" --upstream-sha "$UPSTREAM_SHA"
```

`SOURCE_PACKAGE` needs the real compiler/export evidence required by the native
verifier; a migration inventory cannot substitute for it. `COMPOSED_SOURCE`
must not already exist. For the final `verify-overlay`, `UPSTREAM` must match the
newly integrated manifest. These commands do not start services or admit images.
For Docker exports and complete compiled-test evidence, use the exact
[export v2 contract and JSON examples](codex-export-evidence.md).

## Preserved integration contracts

| Upstream change | Platform adaptation |
| --- | --- |
| Client async resolve/reject request handles | Retain the public synchronous bounded-queue and explicit async methods; route new async facades to async methods. |
| App Server daemon and extension setup | Keep the host runtime installer with new configuration and daemon-settings arguments. |
| Thread startup and parent discovery | Keep host-reserved identities, immutable tool ceilings and durable-store requirements with new parent resolution. |
| Responses metadata replacement then patch | Carry both through recovery under the reserved turn ID; keep the existing replacement-only recovery API. |
| Cancellable tool-start lifecycle | Race the policy gate with cancellation and retain effect classification, denials and blocked-call completion. |
| Typed contextual prompt fragments | Keep exact read-only text, `ai_platform.readonly_context` classification and harness provenance together; register no write tools. |
| Guardian compaction review | Keep new unusable/oversized-checkpoint coverage and the platform tool-effect field. |

Native Codex remains the single model/tool loop. Tenant authorization, durable
history, Gateway leases, capability dispatch and Worker isolation retain their
current owners. Upstream feature availability does not enable a feature in the
platform integration automatically.

## Verification ladder

The [migration receipt](../../reports/harness/codex-upstream-20261017/receipt.json)
records source adaptation and static checks. The program ledger owns later
integrated build and product evidence. Each result retains its own limits.

Run the affected Core, App Server/client and extension tests in the controlled
composed workspace through the prescribed Docker workflow. Include original
recovery/suspension and host-startup tests, metadata-patch recovery, typed
read-only context, tool-policy denial/cancellation and Guardian regressions.
Also run the existing entry points:

```sh
AI_PLATFORM_AGENT_RUNTIME_SOURCE="$UPSTREAM" make agent-runtime-recovery-tests
AI_PLATFORM_AGENT_RUNTIME_SOURCE="$UPSTREAM" make agent-thread-store-contract
make harness-check
make agent-runtime-source-contract
```

The recovery entry point runs Runtime/Worker library tests and scoped Core
recovery tests; separately affected upstream package tests remain required.
Complete the [integration and rollback](integration-and-rollback.md) ladder with
new images: real provider text, resumed/recovered history, read-only/write
capabilities, root/child policy and budget inheritance, tenant boundaries,
browser interaction and rollback. Preserve volumes and singleton ownership.
Schema epoch equality does not establish old-history decoder compatibility.

## Independent Python Sandbox image admission

The current source lock has four distinct artifacts: App Server, Runtime,
capability Worker and `python_sandbox` (`ai-platform-python-sandbox`, protocol
`sandbox-job/v1`). Source refresh creates all four canonical protocol records
and revokes every old image identity. A source-only or partially built lock
does not establish a complete release.

Record each verified image separately with `source-lock record-local-image`.
Use `--artifact python_sandbox` for the independent sandbox. Its OCI labels use
the existing `com.misaya.ai-platform.capability-worker` namespace, with exact
`artifact=python_sandbox` and `binary=ai-platform-python-sandbox`; the control
Worker's labels cannot satisfy the sandbox check. The sandbox must carry the
same immutable upstream revision, full overlay digest and capability schema
digest as the source lock.

Verify the complete service-image release unit before deployment:

```sh
platform-tools source-lock verify-local-images \
  --repo-root . --lock deploy/agent-runtime-source/lock.json \
  --runtime-image "$RUNTIME_IMAGE" --worker-image "$WORKER_IMAGE" \
  --sandbox-image "$SANDBOX_IMAGE"
```

This gate requires three distinct actual image identities, locked runnable
artifacts, source/label/digest agreement and one platform. Published images also
need their own exact locked registry reference in Docker's `RepoDigests`.
The default sandbox tag is
`ai-gateway-agent-python-sandbox:local-<upstream12>-<overlay12>`.

An exact old `092d3acd…` three-artifact lock remains readable by `validate`
without `--require-*` flags, as `historical_audit_only: true` and
`release_admission: false`. It cannot record images or satisfy a current image
release gate. Current locks must include the sandbox even for structural
validation. A per-artifact `--require-artifact` result proves that artifact;
complete release admission still needs the three-image gate.
