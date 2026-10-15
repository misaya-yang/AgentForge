# Authority ledger ACL revision v1

The frozen baseline and epoch files are immutable. `ledger_acl_v1.json` is a
separate digest-pinned bootstrap contract. Fresh installation applies it after
verifying the original baseline and before writing the adoption marker.

An already-adopted database uses the explicit admin-only command:
`python -m database.authority bootstrap-ledger-acl`
The command reads `AI_GATEWAY_DATABASE_ADMIN_DSN`, holds the existing migration
advisory lock, proves all pre-bootstrap fingerprints, and emits a nonsecret JSON
receipt. Normal migrate/status/verify still use the separate migrator DSN; none
retry as admin. Never point the repair at shared data without its own approval.

## Minimal matrix

Only the migrator gains public-schema USAGE, without CREATE or grant option.
Only these named ledger rights are added:
- platform_schema_baselines: INSERT, SELECT
- platform_schema_changes: INSERT, SELECT
- platform_schema_change_attempts: INSERT, SELECT, UPDATE

No DELETE, TRUNCATE, REFERENCES, TRIGGER, schema CREATE, application data grants,
PUBLIC grants, new memberships, or application-role grants. The owner remains
NOLOGIN. Schema/condition reads and DDL borrow owner within bounded scopes;
ledger DML executes after restoring the connected migrator role.

A canonical JSON version/digest/role-prefix receipt is stored as the baseline
ledger table comment. Missing/tampered receipts and extra table/column grants,
grant options, unexpected owners/grantees/grantors, or partial grants fail closed.

Raw fingerprints remain raw. Compatibility verification requires exactly the
manifest's canonical public-USAGE line and proves every remaining ACL line hashes
to the unchanged frozen baseline/epoch ACL. The derived receipt reports original
and effective ACL hashes plus the revision digest. All other fingerprint classes
must match exactly. This is an exact additive proof, not permission drift ignored
by filtering patterns. Frozen artifacts never change.

The ACL repair cannot be an ordinary next epoch: the previous migrator cannot
read its ledger to discover that epoch or record its success. It is an explicit
versioned bootstrap step preceding epoch discovery.

## Python startup compatibility

The native Gateway plus Python Knowledge API/worker hybrid supports canonical
`2026_08_post_kb_v1` epochs **5 through 17** for Knowledge. Epoch 5 is the floor
because dataset reads include `is_archived`. The explicit Knowledge window in
`ai_gateway_contracts.database_revision` is enforced by Knowledge's database
connection guard and by `startup-check --principal knowledge_api` or
`--principal knowledge_worker`. Helm uses the same principal-specific check.
The startup check is read-only and does not replace authority fingerprint checks.
A future epoch must be reviewed before increasing the ceiling.

The intervening migrations were reviewed for the Knowledge storage contract:

| Epoch | Change | Why Python Knowledge remains compatible |
| --- | --- | --- |
| 011 | Scheduled tasks and execution receipts | New Gateway tables and Gateway-only grants; no Knowledge objects or grants change. |
| 012 | Tenant plugin lifecycle | New Gateway policy table and Gateway-only grants; admission is enforced by native Gateway. Knowledge does not perform that admission. |
| 013 | Scheduled notification acknowledgement | Nullable column, terminal-state constraint and indexes on the new execution table; no Knowledge mutation. |
| 014 | SDK session cleanup | New Gateway barriers/outbox and Gateway-only grants; Knowledge does not own SDK sessions or cleanup. |
| 015 | SDK capability authority | New Assistant SDK tables/functions and Gateway/Runtime/Worker grants; changes to existing dispatch functions add SDK-engine branches while preserving the Runtime branch. No Knowledge objects or grants change. |
| 016 | SDK usage audit lookup | Non-unique partial index on Gateway audit logs; preserves existing rows and insert/update semantics, including Knowledge's retained audit insertion right. |
| 017 | SDK execution lineage | Replaces the SDK-only issuance function with the same signature and privileges; permits a bounded empty selection. Knowledge never calls this function. |

The legacy Python Gateway retains its separately reviewed **2 through 10**
window. This is not a full-native-feature backport: Python Gateway does not enforce
epoch-012 tenant plugin bindings or epoch-014 SDK deletion receipts. It must not
serve a database using these native features or be selected as a rollback target
for such a deployment. Retain a compatible native Gateway and the paired
Runtime/Worker. The default authority startup check keeps the legacy ceiling;
selecting a Knowledge principal changes both its floor and ceiling, never the
migration plan or connected role. The Python CLI also retains its previous
conservative window for `runtime` and `capability_worker`; this does not describe
or change the actual Rust services' own startup compatibility checks.

Legacy Compose currently waits for migration success but does not run the Python
Gateway epoch check inside `DatabaseStorage.connect`; Helm has an init check.
Consequently the declared Gateway window alone is not a Compose fail-closed
control. This existing gap is separate from enabling Python Knowledge on the
native schema and must be closed before claiming legacy Gateway startup safety.

## Inactive canonical018–020 successor source plan

The consolidated [successor-chain design](../../docs/design/canonical-schema-successor-chain.md)
and exact source plan connect the metadata reader, Knowledge42501 trigger repair
and Gateway-only delivery ledger in that order. They remain outside migration
discovery; canonical017, current windows and ACLv1 are unchanged. The source-only
checker may emit an exclusive external preview, never apply it. Missing measured
018/019/020 fingerprints and actual-role gates block authority/application activation.
Do not independently activate the recovered018-only reader/ceiling patch.
