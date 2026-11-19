# Native Gateway and Codex harness compatibility

## Source ownership and scope

Codex is the sole Agent loop, compaction and child-agent owner (ADR-006). Native
Gateway owns authenticated capability projection, model transport and tenant
policy. The recorded integration revision is
`279ba894152b2c01c5294cc0723b463b209bdca4`; this repair does not update that pin.

Official GitHub verification on 2026-10-12 found upstream `main` at
[`a9b0815167c27ab13719dd12d13b63e02b70d221`](https://github.com/openai/codex/commit/a9b0815167c27ab13719dd12d13b63e02b70d221),
committed 2026-10-11 23:44:43 UTC. The
[immutable comparison](https://github.com/openai/codex/compare/279ba894152b2c01c5294cc0723b463b209bdca4...a9b0815167c27ab13719dd12d13b63e02b70d221)
reported 941 commits ahead, zero behind, with the pinned SHA as merge base.
That observation is not an approved upgrade target or a promise of latest parity.
The API's returned changed-file list is capped and cannot stand in for a full
three-way source inventory.

## Executable namespace contract

The [pinned upstream DynamicToolSpec](https://github.com/openai/codex/blob/279ba894152b2c01c5294cc0723b463b209bdca4/codex-rs/protocol/src/dynamic_tools.rs)
uses tagged function or namespace variants. `runtime_contract::dynamic_tools`
groups explicit descriptor `metadata.namespace` values into canonical namespace
objects. `tool_policy` selects those exact identities. `model::provider_body`
flattens each identity to the existing hash-qualified provider alias and supports
explicit namespace/name forced choices. No extra loop or process is introduced.

Empty/default `functions` namespaces stay plain; sources are not namespaces.
Duplicate identities, upstream/platform-reserved namespace names, non-string
namespace metadata and malformed new namespace identifiers fail closed. Existing
flat catalog projection is preserved. Namespaced leaf names that happen to equal
kernel controls remain independent capabilities. The new namespace bridge keeps
its 64-byte identifier bound, matching the existing provider transport. The
broader catalog identifier grammar (160 characters including dot/colon/slash)
was already incompatible with App Server (128 ASCII alnum/underscore/hyphen)
and provider (64) limits; this repair does not silently rename those configured
identities. All 22 bundled capability names satisfy the current 64-byte grammar;
configured catalogs still require their existing contract validation. Namespaced tool catalogs receive a versioned
fingerprint because previously created threads lost namespace identity. Existing
plain catalogs retain their digest, including exclusion of per-turn attachment
references. The immutable Runtime ceiling is never rewritten in place.

Runtime descriptor resolution uses explicit namespace metadata ahead of legacy
source aliases. Final authorization still checks capability ID, kind, version,
schema, tenant snapshot and live lease. Runtime/Worker tools and Codex native
collaboration continue through their existing owners and approvals.

The current bundled Worker V2 registry and its native catalog converter still
produce flat descriptors. This compatibility path consumes authorized explicit
namespace metadata; it does not itself add namespace configuration to that
registry or an admin UI. Legacy source alias collisions remain rejected.

## Verification and a real revision synchronization

External native tests cover grouping, distinct same-name tools, exact per-turn
policy, duplicate/reserved/malformed identities, legacy fingerprint stability,
changed namespace fingerprints, provider alias selection and denied ambiguity.
External Runtime tests cover exact resolution, plain/namespaced collisions,
legacy aliases and invalid metadata. They require their respective supported
native or Docker/hosted harnesses; storing tests is not a pass.

A revision upgrade follows the existing
[refresh plan](../plans/codex-harness-refresh-2026-09-22.md) and
[source lock process](../../deploy/agent-runtime-source/README.md): freeze a
reviewed immutable target; obtain its clean full upstream source; inventory every
overlay path against old and target upstream blobs; replay only platform seams;
resolve Cargo in the permitted builder; generate protocol schemas and SBOM from
the composed source; refresh source receipt and overlay manifest; build Runtime
and Worker as a pair; verify actual image identity; exercise start, resume,
compaction, cancellation, approval, children, providers and rollback. Native
Gateway tests alone do not establish that revision synchronization.

Changing the Runtime resolver also requires refreshing the overlay/source unit
and rebuilding both images before the repair can be released. Do not retain
`candidate_start_allowed=true` for old images against new source hashes.
