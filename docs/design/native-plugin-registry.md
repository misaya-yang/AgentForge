# Native plugin registry and tenant lifecycle

The native Gateway exposes `GET /api/v1/assistant/plugins` and
`PUT /api/v1/assistant/plugins/{plugin_id}/lifecycle` through its existing
Assistant router and active JWT authority. The deployment-selected validated
capability catalog remains the source of executable capability attribution.
Gateway owns tenant admission preferences; Runtime and Worker retain execution.

## Registry contract

The additive response retains `schema_version: ai-platform-plugin-registry/v1`,
the numeric `catalog_revision`, and permission-filtered `plugins`. Plugins and
exact version labels are sorted lexically; capabilities are sorted by name.
No semantic-version ordering, automatic upgrade or latest-version claim is made.

Each version includes `availability`, permission unions, and capability identity,
version, schema hash, category, risk, read-only classification and availability.
Required permissions use the existing tier/role permission lattice. Optional
declared `permissions` are informational; they never grant access. A nullable
capability version is projected as the existing native bridge label `"null"`.
Package text, arbitrary plugin metadata, file paths and credentials are absent.

New registry fields:

- `catalog_revision_token`: exact unsigned revision as decimal text. Mutations
  use this token rather than the potentially imprecise JavaScript number.
- `tenant_id`: the currently authenticated tenant. This API cannot select a
  different tenant through a URL parameter, query or body.
- `can_manage`: current exact `admin` role or case-insensitive `admin` tier.
  Selecting a version also requires all that version's existing permissions;
  an admin role still cannot bypass an explicitly higher required tier.
- `package_installation_supported: false`: the tenant API cannot install packages.
  Deployment-owned compiled-executor pack management is described separately in
  [Native plugin packs](native-plugin-packs.md).
- Per-plugin `lifecycle`: `managed`, configured `enabled`, nullable
  `selected_version`, exact decimal `revision`, nullable historical
  `selected_catalog_revision`, `selection_status`, and nullable `updated_at`.

With no explicit binding, lifecycle is unmanaged, enabled, revision `"0"`, with
status `deployment_default`. Existing deployment-default behavior is preserved.
With a binding, status is `current`, `catalog_changed`, or `version_unavailable`.
Configured enablement is not runtime readiness or permission approval. The saved
selected version is null for a non-admin who cannot see that version. Admins can
also inspect saved orphan bindings whose plugin has entirely left the catalog;
these entries have no versions. Current catalog capabilities remain filtered by
the existing permission lattice for every caller, including administrators.

## Tenant selection and enable/disable

The strict JSON update contains exactly:

```json
{
  "selected_version": "1.2.0",
  "enabled": true,
  "expected_revision": "0",
  "expected_catalog_revision": "7",
  "expected_tenant_id": "example-tenant"
}
```

`selected_version` must identify an already-registered deployment version when
enabling. A previously selected unavailable version can be disabled or remain
disabled. No request can create a package, set permissions, choose an arbitrary
download URL, or write another tenant. `expected_tenant_id` is only an identity
precondition against the JWT-derived tenant; a mismatch returns 409 and never
changes the target. This prevents a stale editor from acting after a tenant
switch. Other additional request fields are rejected.

The server checks the exact catalog revision and tenant binding revision. An
absent binding requires revision zero. Existing rows are locked and incremented
on a successful update; racing or stale writers receive 409. Decimal tokens are
canonical and reject leading zeros, signs and out-of-range values. The UI reviews
the tenant, version and enablement before saving, prevents repeated clicks, and
requires refreshing after a conflict or uncertain save. A failed refresh after
commit is shown separately from a failed write; no optimistic success is inferred.

The console owns pending and refresh-required lifecycle attempts at the page
lifetime, rather than in filtered plugin cards. Search, availability filters and
registry-driven card remounts cannot discard an in-flight or uncertain-write
fence within the mounted authenticated page. Route leave/reload starts a new
read lifetime; this is not a durable client operation ledger. A successful registry reread releases only attempts that had settled before
that read started; an older or background read cannot clear a later outcome.
Successful explicit rereads require a fresh review and acknowledgement. Failed
rereads preserve the outcome message and block replay, including after filtering.
The page is bound to the existing authenticated read scope; old-session handlers
and completions cannot start a write, refresh the replacement view, or invalidate
its tool catalog. Current registry identity and the reviewed descriptors are
rechecked before submitting the unchanged tenant/catalog/revision CAS payload.
These are controlled-client recovery guarantees, not proof of live commit
atomicity, browser behavior, or full TL-05 acceptance.


The new `gateway.tenant_plugin_bindings` table is an authoritative policy record,
not a second executable registry. It records the selected version, descriptor
fingerprint, source catalog revision, actor, timestamps, and bounded historical
capability names. It permits at most 1,024 bindings per tenant and 1,024 retained
names per binding. Configuration state and a metadata-only `config_changed`
event (`resource_type=tenant_plugin`, `action=set_lifecycle`) commit in the same
transaction in the existing `gateway.audit_logs` table. No event is claimed on
rollback. Tenant-scoped advisory locking serializes first inserts and bounds;
row locks protect revision comparisons. Database timeouts bound lock waits.

## Admission enforcement

Every native `/assistant/tools` read, plugin availability projection, and native
Gateway-to-Runtime capability catalog resolution loads current tenant bindings.
Lifecycle denials are intersected with the existing tool allowlist, blocklist,
categories and RBAC checks. Existing policy arrays are not rewritten, so enabling
never removes a manual deny or changes quotas, required permissions or approvals.
`/assistant/policies` remains the existing base-policy projection; it is not a
flattened representation of lifecycle state. The plugin registry exposes that
separate admission state explicitly.

An enabled managed plugin permits only its selected, fingerprint-matching
version. Disabled plugins, changed selected contents, and unavailable selections
deny all their known capabilities. Hashes cover canonical validated descriptor
data, including admission requirements. Reusing a version label with changed
content requires explicit reselection. Old names remain denied after catalog
removal, preventing an older Worker catalog from silently restoring access.
Unattributed deployment capabilities retain their existing behavior.

Storage failure, missing migration, malformed attribution or invalid saved data
fails closed. Neither policy nor capability discovery silently falls back to
unmanaged mode. These changes apply at future catalog admission boundaries;
they do not revoke previously signed execution leases or cancel an active turn.
Runtime/Worker approval and signed-snapshot authority remain mandatory. The
separate deployment pack path adds active-generation checks without weakening them.

## Installation and activation boundary

Tenant lifecycle saves admission preferences; deployment activation now has a
separate offline, Worker-owned path documented in [Native plugin packs](native-plugin-packs.md).
The Worker image ships `ai-platform-plugin-packs` for pinned local Agent Plugins
1.0.0 artifacts that bind only existing compiled executors. It validates exact
archive and executable digests, installs inactive bytes, stages matching immutable
Worker/Gateway generations, and revalidates prior packs during rollback. No public
installer, remote-code loader, package-defined executor or new MCP authority is
introduced. Actual implementation changes still use the ADR-006 immutable OCI
release path.

Worker catalog headers and descriptor identities must match the selected Gateway
generation, including when every pack is disabled. Current Worker health exposes
an activation receipt; stage success is not evidence that services restarted or
that a capability executed. Each activation requires tenant reselection for
previously managed plugins because their selection fingerprints change. The
bundled data-only packages remain inert until explicitly released, approved,
installed and activated through that path. An empty registry remains valid.

## Schema, errors and migration

Public request/response schemas are in `sdk/openapi.json`. GET returns 200;
missing/invalid JWT identity returns 401 and unavailable authorities or catalog
policy return 503. API keys are not accepted. PUT additionally returns 403 for
insufficient current authority, 409 for a stale revision/unavailable selection or
tenant binding bound, and 422 for invalid strict JSON or identifiers. All errors
use `{detail: "..."}` with redacted messages. A 503 during commit can represent an
uncertain outcome; clients must reread before retrying.

Canonical epoch 012 adds the tenant policy table and grants only SELECT, INSERT
and UPDATE to the Gateway role. There is no DELETE endpoint or package uninstall.
Apply it through the existing canonical migration authority before starting new
Gateway readers. Drain older Gateway readers before lifecycle writes: old
binaries ignore this policy and are unsafe rollback targets after restrictions
are configured. Preserve a compatible enforcing Gateway or forward-fix.

## Verification status and consolidated acceptance

The preceding tenant-lifecycle snapshot passed source integration and native
workspace checking on 2026-10-12. The newly added pack-activation source has not
yet been compiled or executed. The
canonical epoch012 and013 records were measured in isolated authority databases,
including repeated verify/migrate/verify, an independent empty-database fresh
chain and service-role ACL checks. These prove schema compatibility only.
Current binary linking and application lifecycle acceptance remain pending;
frontend execution and actual Worker activation remain unverified. The compiled-
executor activation implementation has source tests and a separate deployment
contract; Docker/CI compilation and live execution evidence are still required.

Consolidated gates: the owning native Rust changed-crate gate, frontend
type/lint/test/i18n/build gates, canonical migration fresh/replay/ACL verification,
and `make harness-check`. Required behavioral checks include:

1. JWT-only current-tenant ownership, non-admin rejection, unknown-field/tenant
   injection rejection, explicit tier requirements and hidden-version visibility.
2. Two concurrent revision-zero and existing-revision updates produce one winner,
   with exactly one matching audit receipt and no partially committed state.
3. Enablement cannot override manual tool/category/RBAC restrictions; only the
   selected version is eligible in both Assistant and Runtime catalog responses.
4. Disabled, removed and changed-content versions stay denied after Gateway
   restart; unavailable DB/migration and malformed data remain 503, never allow-all.
5. Stale catalog, stale revision, network loss after commit, refresh failure,
   repeated click, Cancel, navigation and identity change UI paths are exercised.
6. Actual Worker descriptor admission is checked with a registered test deployment;
   an empty bundled registry proves neither activation nor execution readiness.
