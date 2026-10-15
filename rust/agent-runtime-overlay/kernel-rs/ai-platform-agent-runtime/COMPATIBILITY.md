# Managed Runtime compatibility candidate at upstream 092d3acd

This externally staged source candidate targets official stable release
`rust-v0.162.1`, commit `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`.
It has not passed the prescribed Docker/hosted-CI build or release gates; the
production pin and existing artifact admission must not be advanced from this
source document alone.

For Codex-managed runs, upstream Codex App Server owns the only agent loop. This crate owns
PostgreSQL persistence, immutable Gateway capability ceilings and private HTTP
adapters. The host-only start/resume options install `ToolPolicy` before Core
captures extension data. Ordinary turns may narrow their authenticated snapshot;
they cannot widen the original thread ceiling. Configless verification reuses
that ceiling and does not reconfigure native search. Legacy rows without a
ceiling need an explicit authenticated policy before resume.

## Persistence

Thread membership, JSONB creation projection and canonical session metadata are
committed in one PostgreSQL transaction. Rollout, metadata and platform receipt
writes await commit. There is no background write queue. Any failed history, metadata or receipt write poisons
the thread in the current Runtime process: subsequent persist/flush/shutdown,
turn admission and tool dispatch fail. Terminal admission also checks this
fence: recovered PostgreSQL can persist an explicit failed terminal; an ongoing
outage produces the existing non-durable failed terminal, never success. The host-only Core durability marker
propagates flush failures before ordinary or compaction model sends. Recover by
reloading committed history; do not clear the fence after a successful read.

New creator and workspace fields are stored in existing JSONB fields. The
creator user comes from the authenticated platform membership; a platform tenant
is never a ChatGPT account. Creator/originator metadata is fill-only. Initial root startup data is staged only after durable root authorization and
committed with the creation projection; resumed/forked/child startup resolves the
original ceiling and canonical catalog through exact PostgreSQL membership.
Resume returns an authoritative freshly loaded PostgreSQL replay snapshot when
the caller supplies a canonical session header. Missing or unrecognized snapshot
revisions never authorize cache reuse. Explicit import/override histories retain
their caller-owned identity, after visibility and sticky-write-failure checks.
This adapter issues no snapshot revisions. `SubagentSpawn` persistence uses the
same synchronous commit and sticky-failure barrier as other checkpoints.

Legacy flat dynamic-tool metadata is normalized when decoded. Pending
host metadata remains in memory until a successful metadata update. Discard only
releases staged metadata and preserves committed history.

Deletes are idempotent tombstones. A root delete hides its full membership;
subagent deletes hide their descendants. Writers and deletes share the root row
lock, including lifecycle and V1 receipts. Append-only history and platform
artifact/attachment ACL records remain retained for audit. New native Codex
thread attachment/project/section APIs retain `Unsupported`; existing platform
attachments and Worker capabilities remain supported.

## Native history and rollback

The history format is the complete native upstream `RolloutItemWire` at
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`. No platform filter converts target
events to user messages. Target compaction adds authoritative resume metadata,
and retained assistant delivery has upstream compatibility encoding. Same-database
rollback to `279ba894` has not been validated; preserve full target history and
require a stop-write, backup-and-restore path until the N-1 decoder and recovery
matrix proves otherwise. The older `94cbbdda` binary already cannot decode the
previous release's token-usage/retained-context records. The release coordinator
owns the complete service/database restore receipt; schema epoch equality does
not demonstrate decoder or replay compatibility.

## Hosted feature profile

The isolated Runtime home receives the full upstream bundled model catalog with
`use_responses_lite=false`, supplied through `model_catalog_json` so reload and
children use the same Gateway-supported Responses wire. Unknown model fallback
already uses standard Responses. Native Code Mode, reasoning overrides,
Guardian automation, hooks, Apps, plugins, realtime conversations, daemon
startup, worktree creation, system proxy fallback and implicit TTY selection are
not enabled by this integration. Platform Worker tools and Codex collaboration
remain governed by their existing authenticated catalog.

Dynamic-tool ownership uses the immutable thread catalog with namespace/name
identity. Child capability authorization resolves the exact durable
`TurnContextItem.root_turn_id`, checks the root snapshot and live lease, and
qualifies execution call IDs to avoid sibling collisions. Codex replies and
child lifecycle receipts retain their original native identity; root cancellation
propagates through child execution tokens. Spawn-tool completion is acceptance;
actual subagent terminal activity is projected separately.

## Stable-target host seams

The target's embedded network policy is loaded and activated before caller-owned
clients are constructed. EnvironmentManager and embedded App Server share that
policy so later revocation reaches both. The target authentication bootstrap,
originator scope, agent-membership admission and selected-environment requests
are retained. Reserved turn IDs and recovery carry target `WithTurnExtensionData`
and root-turn lineage in addition to signed Gateway model-lease metadata.

The host's durable dispatch gate consumes `ToolStartInput`; the target now embeds
a lazy, Send-but-not-Sync permission future, so borrowing the entire input across
an async gate is invalid. A separate input with the same immutable snapshot is
constructed for the upstream start observer, retaining its lazy permission
semantics. The target's synchronous dispatch observer remains available.

The obsolete `guardianv2.thread_context` flag is removed from the hosted profile;
Guardian automation remains disabled. New stdin approval protection remains
enabled and the private approval broker rejects unsupported stdin writes. New
API-key catalog discovery is inert for the custom Gateway provider without a
catalog URL. Native defaults do not widen the immutable tool ceiling.

## SDK capability authority composition

The paired epoch-15 SDK authority source is included in this candidate. Genuine
OpenAI/Claude SDK runs use separate durable SDK-run ownership and invoke the
private SDK capability route. They do not fabricate Codex thread/turn identities,
model leases, DynamicToolCallParams or process-local approval waiters. The Runtime
hosts capability authority, not a second SDK model loop.

SDK requests reuse the same explicit namespace descriptor resolver, durable
exact-action approval persistence and signed Worker execution transport. Existing
Codex wrappers retain their original thread/turn validation before delegating to
the shared transport. The Worker has a distinct SDK admission branch; the original
Codex engine predicate is not widened. Tenant/session/run ownership, immutable
snapshot, arguments, effect, owner fence, approval and original receipt remain
bound together. Unknown outcomes block further governed actions and do not justify
retrying an external write.

This source must ship with the complete reviewed Gateway/SDK worker authority
change and epoch-15 database grants/migration. No schema fingerprints, durable
concurrency, provider behavior or Runtime/Worker compilation are proven by this
composition. The root-owned `docs/architecture/sdk-capability-authority.md` owns
the SDK authority and recovery contract.

## Verification

Rust unit coverage includes policy omission/narrowing/widening, namespace
identity, native search authority, Gateway model catalog mode, subagent activity
and child execution identity. The PostgreSQL contract test injects an append
failure, restores healthy database reads and requires every durability barrier
to continue failing. These tests require the coordinated Docker/hosted Rust
build; source inspection and `git diff --check` alone are not execution evidence.

## Explicit dynamic-tool namespaces

The native Gateway projects descriptor `metadata.namespace` into the pinned
upstream canonical `type: namespace` / nested `tools` structure, and emits the
same namespace/name pair in `allowedTools`. An empty namespace or `functions`
means the default namespace. Descriptor sources are provenance, not an inferred
namespace. Duplicate identities, reserved namespaces and invalid identifiers are
rejected before thread creation for the new namespace projection. Existing flat
catalog projection is preserved. The current provider bridge bounds explicitly
namespaced identifiers to 64 ASCII letters, digits, underscores or hyphens.
Namespaced capabilities may share a leaf name with a native control tool;
only default and kernel-owned namespaces are treated as native controls.

At Worker dispatch, explicit namespace metadata is authoritative. Plain calls
cannot select namespaced descriptors, and a descriptor's source cannot override
its explicit namespace. Descriptors without namespace metadata retain the
historical source alias for old serialized calls. Same-named tools in distinct
explicit namespaces therefore resolve independently. This changes neither the
signed capability ID/version/schema checks nor the durable lease check.

Existing flat catalog fingerprints stay byte-compatible. Catalogs with an
explicit nondefault namespace use the `codex-namespaces/v1` fingerprint domain;
a thread created with the old flattening adapter must be recreated instead of
silently widening or replacing its immutable tool catalog. Namespace-aware
forced function choices resolve to the provider's collision-safe wire alias;
ambiguous bare names remain rejected.

This is a compatibility repair at the existing pinned revision, not a revision
upgrade. Changed overlay bytes invalidate prior source/image identity. Refresh
the composed-source receipt and both Runtime/Worker images through the existing
Docker or hosted CI release process before runtime acceptance. Source review and
external test definitions do not establish that the new binaries pass.
