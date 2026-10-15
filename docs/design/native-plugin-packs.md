# Deployment-owned Agent Plugin packs

## Scope and authority

The Worker image now ships `ai-platform-plugin-packs`, an offline installer and
activation planner for existing Agent Plugins 1.0.0 packages. A pack can bind only
reviewed, already-compiled Worker capability IDs. It cannot provide a binary,
script, subprocess, URL, credential, new permission, MCP grant, or executor.
Package Markdown remains inert files; this operation does not install prompts or
claim that plugin-defined skills/agents are active in the kernel.

Executable implementation upgrades continue through the immutable Worker OCI
release governed by ADR-006. The trust policy pins the exact Worker executable's
SHA-256 as well as each compiled descriptor and each package archive. The running
Worker hashes its own executable before admitting the activation. A changed image
must pass the existing OCI supply-chain gates; a changed Worker binary requires a
new reviewed trust policy and activation. Digest pinning is local operator trust,
not a package-signature verification claim.

This implements the compiled-executor branch of the accepted extension boundary.
Existing governed MCP connections continue through their current registry,
credential, discovery and invocation authorities. A `mcp.json`, stdio command or
remote executable inside a pack is rejected; this CLI does not create an MCP
connection or bypass its approval. Generic remote MCP writes remain outside this
pack feature.

Runtime catalog admission now uses the same permission lattice as the Assistant
registry: an admin role bypasses role/named-permission checks, but never an explicit
required tier. Invalid caller tiers fail closed. This fixes the former Runtime
catalog admin shortcut that disagreed with tenant lifecycle selection.

The deployment operator owns these commands. There is no public HTTP installer.
The tenant plugin endpoint's `package_installation_supported: false` continues to
mean installation is unavailable through that tenant API. Tenant selection and
enablement still use `/assistant/plugins/{id}/lifecycle`, with the same JWT,
permissions, revision, audit and deny-intersection contracts.

## Artifact and provenance contract

Use the existing `plugin.json` schema URI and portable field types. Add a bounded
host extension containing only compiled IDs:

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
  "name": "ai-quiz",
  "version": "1.1.0",
  "description": "Reviewed quiz capability binding",
  "extensions": {
    "com.misaya.ai-gateway": {
      "worker_capabilities": ["generate_quiz"]
    }
  }
}
```

Use a new exact version for changed bytes; never rewrite an existing published
version such as the bundled `1.0.0`. This extension neither grants capability
permissions nor overrides schema, effect, approval, timeout or implementation.
Executable pack names follow the portable lowercase name limit of 64 bytes;
exact version labels are 1–64 ASCII letters/digits plus dot, underscore or hyphen.
No semantic-version ordering or automatic latest-version selection is inferred.
The allowlist's capability keys must match the manifest exactly. Every descriptor
fingerprint is recomputed from the host definition before activation.

Artifacts are uncompressed USTAR, at most 8 MiB, 128 entries, and 1 MiB per regular
file. `plugin.json` is additionally capped at 64 KiB. Only UTF-8 `plugin.json`,
`LICENSE`, and Markdown assets are accepted. Paths are ASCII component names, at
most 99 bytes; absolute, parent, dot, empty and backslash components are rejected.
Links, special files, sparse files, PAX/GNU extensions, USTAR prefixes, duplicate
names, file/parent conflicts, nonzero padding and appended/concatenated archives
are rejected. Extraction never honors archive modes or invokes archive code.
Duplicate JSON keys are rejected at every nesting level. Skills and agent body
frontmatter are not interpreted by this installer.

For example, produce a release artifact with a USTAR-capable release tool, naming
entries without a leading `./`. Review it and independently retain its digest in
the release provenance record. The installer does not download artifacts or
create/update trust on the operator's behalf.

## Trust policy

The policy is a separately controlled, read-only deployment input. It has this
shape; the digest placeholders must be replaced by actual reviewed release data:

```json
{
  "schema_version": "ai-platform-plugin-trust/v1",
  "worker_binary_sha256": "sha256:<64 lowercase hex digits>",
  "managed_capabilities": ["generate_quiz"],
  "packages": [{
    "id": "ai-quiz",
    "version": "1.1.0",
    "artifact_sha256": "sha256:<64 lowercase hex digits>",
    "capabilities": {
      "generate_quiz": "sha256:<compiled descriptor fingerprint>"
    }
  }]
}
```

`ai-platform-plugin-packs fingerprints --worker-binary /usr/local/bin/ai-platform-capability-worker`
prints the actual executable digest and compiled descriptor fingerprints without
executing a capability. The CLI and Worker must come from the same reviewed
release. Fingerprints are evidence to review, not automatic approval to trust a
new package. There is no network or secret lookup in this command.

The `managed_capabilities` set is independent of currently approved/active packs.
Removing a pack from the allowlist must leave its capability IDs managed and
denied. Normal activation enforces a nonshrinking set against the previously
published generation. Startup also rejects selecting an older generation that
would discard subsequently managed names. Replacing/resetting the installation
store or disabling pack mode is an authority-changing deployment migration, not
a supported revocation or rollback operation.

The store must be an absolute canonical path in a protected deployment directory.
The store and created subdirectories have private ownership/modes; existing roots
and directories writable by other users/groups or owned by an unrelated user are
rejected. Run management as the same dedicated UID used to read the store in the
Worker. Keep ancestors and trust files operator-controlled. Mount the store and
trust read-only into running services. Do not share them with tenant workspaces.
The CLI is not a security boundary against an operator who can replace the image,
trust file and entire installation store.

## Install, activate, observe

These are deployment commands, not application request actions. Normal secret and
service configuration remains unchanged.

1. Install exact approved bytes:

   `ai-platform-plugin-packs install --store /var/lib/ai-platform/plugin-packs --trust /etc/ai-platform/plugin-trust.json --id ai-quiz --version 1.1.0 --archive /release/ai-quiz-1.1.0.tar`

   Successful status is `installed_inactive`. Import is idempotent for identical
   bytes. The immutable identity ledger prevents a different artifact from
   reusing `id@version`. An incomplete/orphan object cannot be activated without
   its completed-install identity record. No service is contacted or restarted.

2. Stage a complete active set, with compare-and-swap generation:

   `ai-platform-plugin-packs activate --store /var/lib/ai-platform/plugin-packs --trust /etc/ai-platform/plugin-trust.json --package ai-quiz@1.1.0 --expected-generation 0 --writes-enabled`

   Every invocation replaces the complete selected pack set. Omit all `--package`
   arguments to deactivate every managed capability. `--writes-enabled` must
   agree with the intended Worker release configuration; it does not enable any
   running service. Duplicate ownership or an unavailable executor is rejected.

   Output is `staged_restart_required`, with exact immutable `activation_path`,
   `gateway_catalog_path`, activation UUID, catalog digest and generation. An OS
   lock plus generation comparison serializes writers. Both files are fsynced
   before directory publication, then a single desired pointer is replaced.
   Existing generations are never rewritten. A crash before pointer publication
   leaves the previous desired generation or an inert orphan, not partial files.

3. Drain current turns and old Gateway readers. Restart the Worker with all three
   explicit settings:

   - `AI_PLATFORM_PLUGIN_STORE`: the protected store
   - `AI_PLATFORM_PLUGIN_TRUST`: the reviewed trust file
   - `AI_PLATFORM_PLUGIN_ACTIVATION`: the exact returned activation.json path

   Restart the native Gateway with `AI_PLATFORM_CAPABILITY_CATALOG_PATH` set to
   the paired catalog.json path. Missing/partial settings and fixture mode are
   rejected. Do not remove these settings to disable a package; stage an empty
   active set instead. Do not use an old Gateway binary that lacks the activation
   handshake after activating packs.

   A new Worker startup admits only the generation identified by the published
   desired pointer, checked again after artifact and catalog validation. An
   unpublished generation directory or a stale activation path cannot start a
   Worker, even when its managed capability set is unchanged. Read-only `inspect`
   may still validate such immutable files under current trust; inspection does
   not publish or admit them. A later staged generation does not change an
   already-admitted running snapshot. Restart with the newly returned paths;
   rollback must publish a fresh generation through the command below.

4. Observe the running Worker's `/health/ready` and its additive
   `plugin_activation` receipt: activation UUID, generation, catalog SHA-256,
   actual pinned Worker executable digest, and package references. Constructor
   wiring must exist for every active executor, and write capabilities require
   writes enabled. Current Local Node unavailable transports cannot be activated
   through this pack path. Health proves the configured running snapshot and
   database readiness; it does not prove a downstream provider or operation works.

5. The Worker catalog returns `x-ai-platform-plugin-activation`. Native Gateway
   verifies it against the selected local catalog before exposing any Worker
   descriptor. Managed-ID tombstones retain that expectation even for an empty
   active set. Per-descriptor package/activation tags and exact identities must
   also match. Old/new, subset/superset and disabled/active mixtures fail closed.
   The request's model capability revision is never treated as deployment proof.

6. Re-select desired plugin versions through the existing tenant lifecycle UI.
   Each new activation changes managed capability versions and selection hashes,
   including unchanged packs in the same replacement set. Existing managed tenant
   selections therefore become `catalog_changed` and deny until explicitly
   reselected. This also applies after rollback; it is a deliberate review step,
   not a transparent upgrade. Unmanaged tenant policy keeps its deployment-default
   semantics, intersected with actual Worker activation and existing permissions.

7. Exercise a permitted capability through Gateway → Runtime → Worker and verify
   its terminal receipt. Installation, staged status, matching health and real
   capability execution are separate evidence; none implies the next.

After a conflict or uncertain write, use `ai-platform-plugin-packs status` with
`--store`, `--trust` and the intended `--writes-enabled` flag. It returns the
validated desired pointer, not running-state success. `inspect` validates a
specific generation. Do not blindly repeat a mutation after an uncertain result.

## Execution, revocation and rollback

Active capabilities keep the host's executor routing, schema, risk, approvals and
permissions. Their 64-hex capability version hashes a canonical JSON object with
explicit `package` reference and `activation_id` fields. The shared object-only
hash contract rejects a tuple/array, so nonempty activations use this object
envelope to bind the exact package reference and activation UUID. Existing signed leases and PostgreSQL Runtime snapshot version
checks therefore cannot relabel old work as the new generation. The durable
execution resource binding retains the full descriptor and deployment receipt.

Admission and actual dispatch both recheck active identity. A compiled managed ID
cannot fall through to the dynamic governed-MCP path when inactive. Restart
recovery prefers the saved descriptor, validates ID/effect/schema, and never
substitutes a new plugin descriptor for missing/corrupt old state. Invalid or
changed activation work is terminalized without invoking a capability. Existing
already-dispatched write reconciliation still uses durable receipts or
`SIDE_EFFECT_UNKNOWN`; package deactivation never justifies replaying a write.
Already-running operations are not live-cancelled by this offline planner, so drain
before changing generations. Tenant lifecycle rules still do not retroactively
revoke previously signed leases or cancel a running turn.

Rollback uses the same installer and a new generation:

`ai-platform-plugin-packs rollback --store /var/lib/ai-platform/plugin-packs --trust /etc/ai-platform/plugin-trust.json --activation-id <prior UUID> --expected-generation <current> --writes-enabled`

Every prior artifact, immutable identity, trust entry and compiled descriptor is
revalidated against current policy. Revoked versions cannot be resurrected.
Deploy the new paired paths, observe health, reselect tenant bindings and perform
the capability smoke again. Do not switch directly to an old binary, old trust
file or old generation path as a substitute for this rollback flow. No database
schema migration is added by pack installation; existing epoch012 tenant binding
storage remains authoritative.

## Required verification

The startup/publication and nonempty-activation corrections passed 11 file-only
Rust Worker library regressions in an external controlled composition. These
cover current, unpublished and stale generations, disable-all tombstones,
inspection, current-trust rollback, repeated CAS, descriptor identity, and
explicit desired-pointer phase interleavings. They do not start a Worker or
Gateway, prove a real concurrent startup, or establish capability execution.

Build/check/test through the existing serialized Docker BuildKit builder or
hosted CI, or the task-specific isolated cloud exceptions documented in
`docs/harness/runtime-and-secrets.md`. Keep target/cache artifacts outside the
repository. A changed Worker must be rebuilt and its actual executable digest
reviewed before a new trust policy or activation can claim that binary identity.

Other external, unexecuted test-bearing source variants cover strict archives/JSON, immutable version imports,
install idempotency, descriptor fingerprints, disabled writers, activation/CAS,
disable-all tombstones, changed artifacts/catalogs, rollback trust, unsafe stores,
lock contention, dynamic fallback denial, and Gateway generation mismatch.
Required additional integration/live receipts include crash-point publication,
missing/corrupt durable descriptor recovery, stale-generation leases,
SIDE_EFFECT_UNKNOWN handling, managed-ID shrink rejection, concurrent mutation,
matching binary/generation health, tenant reselection and one real capability
execution plus rollback through the complete service chain. New tests and generated checks remain outside the repository in the verification
package; use its test-bearing variants only in an external controlled composition.
A source review or unexecuted test file is not a passed acceptance gate.
