# Fixed033–035 final service schema successors

This extends the source-owned `schema-admission-service-successors` factory. It is
source only: canonical source remains029 until the primary measures and explicitly
activates reviewed bundles. No new verification fingerprint, receipt, physical
partition, cutover, feature enablement or production acceptance is invented here.
The existing030/031/032 SQL **and complete ChangeSpecs** remain byte-identical; their
scoped plans, admission exports and historical proofs remain reproducible.

The Cedar resource audit is already included in030 through the unchanged original
`gateway-cedar-resource-audit.sql`, SHA256
`a64565576fbe5aa8e321bd5e4cc64b0fc7da97bfd82668bbf9e4d58d040748e3`.
It is inherited and verified, never recreated. Gateway retains INSERT only on that
append audit table; audit readers remain a separate operator authorization.

| Sequence | Fixed result | Permission change |
| --- | --- | --- |
|033 | Outbound A2A's five new authority fields; constant-default nullable cancel state plus five explicit NOT VALID format/non-null constraints | Exactly UPDATE(remote_task_id,cancel_state) to prefix-derived Gateway; existing SELECT/INSERT and existing mutable columns remain. No table-wide UPDATE, DELETE or immutable authority-field UPDATE |
|034 | Validate all five033 constraints, then SET cancel_state NOT NULL | None; separate contract and executed N-1 gate |
|035 | Complete22-family Data control plane: all six original SQL inputs, original native writer policies and legacy retention adapters; owner-only operator approval gates; private tables and32 routines | No change to the historical four-schema raw ACL set. The separate measured partition_ops catalog requires exact owner/schema/function grants: read flag + readonly boolean admission to five native roles, six invoker/metadata writer helpers only to Gateway/Knowledge API/Knowledge Worker whose real029 source ACL already includes INSERT; no private directory/table/DDL/maintenance grants |

The Data six inputs are `expand.sql`, `migrate.sql`, `write-compatibility.sql`,
`native-writer-policies.sql`, `retention.sql`, `legacy-retention-adapters.sql` under
`proposals/data-platform/append-partitions/`. All original bytes are hash-bound.
`acceptance-catalog.sql` is a seventh, **read-only**, pinned auxiliary input exported
for the later isolated all-family acceptance. It is never run by migration035.
`partition-authority-policy.json` is also pinned and records all22 families, exact
routine identities/definer flags and role grants. Nothing narrows coverage to audit.

The candidate applies only these reviewed deterministic amendments, preserving the
original Data/Gateway files and their old hashes:

- Derive the new schema owner from CURRENT_USER; no fixed ai_gateway prefix.
- Move original2s/60s SET LOCAL settings into native034/035 ChangeSpec budgets,
  preserving authority ownership of transaction/timeouts.
- Preserve session_id as directory scope alongside tenant_id/user_id. Conflict
  metadata refuses mismatching user/session identities, as well as tenant scope.
- Keep write_json SECURITY INVOKER. A source parser must inspect the function
  header, not mistake a SECURITY DEFINER comment in its body for its declaration.
- The original BEFORE UPDATE directory rewrite could expose a global unique
  collision before the payload's RLS WITH CHECK rejected a foreign scope. The
  candidate records only nonunique transaction/move markers before UPDATE; AFTER
  UPDATE or row-movement AFTER INSERT finalizes global keys after original RLS,
  NOT NULL and CHECK. AFTER DELETE preserves a pending movement, then the same
  directory UPDATE retains original FK ON UPDATE actions. A private partial
  movement index avoids full-directory scans. Real row movement/FK/RLS/concurrent
  acceptance is still mandatory; source guards are not that result.
- Existing owner-executed retention adapters use a separate owner-only flag reader;
  business sessions keep the original-session privilege check. An owner definer
  called by Knowledge Worker cannot accidentally fail through a session_user that
  lacks direct progress-table access, or gain a universal app metadata API.
- Cutover takes the exclusive same-family advisory lock before its family row,
  draining the shared lock held by original writer compatibility calls.
- Expand requires a future UTC boundary and its separately approved exact boundary;
  expand and cutover both require owner-only, immutable, revision/scope-bound
  operator evidence. Migration creates no approvals and invokes neither operation.

033 stages the original proposal's combined NOT NULL addition across033/034;
there is no populated-table rewrite/backfill shortcut or permission-lint ignore.
All necessary UPDATE columns are checked independently by the actual Gateway and
Knowledge startup guards and native admission catalog. A2A's remote message/card
identity columns cannot be updated. Knowledge tenant/dataset/document columns
remain immutable to the pipeline worker.

## Supplemental real catalog measurement

The frozen fingerprint scope remains gateway/assistant/knowledge/public. Adding
partition_ops silently to that algorithm would change historical authority rules.
035 therefore requires a separate actual PostgreSQL supplemental catalog in its
verification reference, covering schema, relations, columns, defaults, constraints,
indexes, functions/deparser bodies/security flags, ACLs, policies, default grants,
and immutable family/statement-policy definitions. Owner/role names are normalized
with the same exact principal mapping; arbitrary-prefix deployments remain valid.

The supplemental query reader belongs to `service_successors/partition_catalog.rs`.
It reads the unchanged catalog query snapshot with an explicit partition_ops scope;
the historical `fingerprint.rs` producer remains byte-identical at SHA256
`704ec8fa6fd0fc782ecf63e84690e02cadf199c3ef35d7306d17f4740ebfc0dc`.
The seven retired-source allowlist, all11 current native replacement bindings and
the original producer descriptor/018–020 receipt identities remain exact.

The real single-step runner captures this catalog after applying035, before writing
a reference. Exact private table/schema/routine grants and all22 families are
required. It then verifies the same supplemental value during ordinary ledger
verification and actual no-op replay. A missing, substituted, extra-grant, overload,
security-definer or changed catalog cannot be covered by only the old four hashes.
Every installed routine's stored body also has a source-derived SHA256 postcondition;
those are source text pins, not guessed database verification fingerprints.

The authority role probe now runs all active source-owned admission queries and
actually requires business-role denial on private tables, state mutation, owner
expand/approval functions and partition schema CREATE. Native process, real tenant
RLS, all writer conflict semantics, original FK actions and recovery remain separate
real acceptance. Existing030..032 reference/proof bytes need no supplemental field.

## Measurement and activation path

Use the same controlled native CLI and external artifact mount as the030–032 plan:

```text
schema-admission-service-successors validate --source-root /source --sequence 33
schema-admission-service-successors prepare --source-root /source --sequence 33 --previous-preview /authority-artifacts/preview032 --output /authority-artifacts/preview033
schema-admission-service-successors measure --source-root /source --sequence 33 --preview-root /authority-artifacts/preview033 --expected-database schema_successor_verify_services033 --run-approved-isolated-measurements
schema-admission-service-successors verify-preview --source-root /source --sequence 33 --preview-root /authority-artifacts/preview033 --measured
schema-admission-service-successors bundle --source-root /source --sequence 33 --preview-root /authority-artifacts/preview033 --output /authority-artifacts/bundle033
```

Repeat for034 using the real measured033 preview, and035 using034. A fresh approved
clone per step is allowed only when the unchanged runner proves the complete exact
predecessor ledger, raw ACL, fingerprints and postconditions. No step can skip an
unmeasured predecessor. Scope hashes exclude unselected future inputs, retaining
all previously approved030..032 plans. The complete current allocation additionally
requires the catalog's exact compiled bytes; selected historical prefixes retain
their independently frozen scope checks. Per-prefix activation proof files are never
replaced. Primary reviews actual new lock pins, archives, supplemental evidence and
Runtime/Worker expected exports; this factory never activates source or a DB.

Static delivery lint recognizes035's dormant installed routine bodies and exact
ACL-only DO blocks only with its full source SQL hash and complete fixed ChangeSpec.
An edited body/spec, appended operation or timeout override loses recognition.
Executed concurrent DDL inside transactional migration remains refused. N-1 is
required for every step; static checks never set release_eligible=true.

## Separate operator phases and hard cutover gates

Installing035 leaves all22 families registered and all physical sources untouched.
Run the native `platform-append-partitions` snapshot/plan flow from its owning
[operator guide](../deployment/data-platform/append-partitions.md). Every physical
stage needs explicit authority/operator authorization and a fresh actual snapshot.
An offline bridge now prepares the **approval call** for the owner-only gate:

```text
partition-stage-review --source-root /source --operation-plan /absolute/native-plan.json --evidence-files /absolute/seven-evidence-files.json
```

The evidence file is exactly a map of `authority_snapshot`, `pitr_restore_drill`,
`role_rls_acceptance`, `n_minus_one_compatibility`, `dependency_rebind`,
`authority_approval`, `native_writer_acceptance`; each value has an absolute regular
`path` and its actual `sha256`. All seven bounded artifact files are read and their
bytes checked. The snapshot must equal the native plan's snapshot digest; cutover's
scope must equal the original native planner's canonical five-artifact digest.
The exact emitted expand/cutover statement, family, revision and connection settings
are checked. Output gives a parameterized `approve_operator_stage` call, its
parameters and bound scope/boundary; it opens no DSN and has no execute mode.

Hash validation does **not** establish an artifact's correctness or live acceptance.
Output explicitly requires operator authorization and reports effects=false,
ga_acceptance=false and release_eligible=false. The real operator verifies the
content/provenance of PITR, role/RLS, native writer, original FK action/dependency
and executed N-1 proofs before recording an approval as canonical NOLOGIN owner.
A revision/boundary/hash/evidence replacement or missing approval is refused.
The expansion/cutover function still performs all original phase/catalog checks.

Keep original bounded typed-PK backfill, online legacy validation, every inbound FK
expand/validate/rebind phase (including composite scope/actions), actual matching
legacy index construction, bounded future partitions and preservation of triggers,
column/table ACL, RLS/FORCE RLS and sequence ownership. Cutover never performs
backfill, validation scans or missing index builds under its exclusive lock.
CREATE INDEX CONCURRENTLY and DETACH CONCURRENTLY/FINALIZE are separate native
operational phases outside transactions, with exact2s/60s/UTC settings and recovery
observation after each command; no migration executes strings returned by routines.

Directories keep global idempotency keys after payload drop. Retention still needs
active-work/lease/reference barriers, real archive/restore receipts, interrupted
DETACH recovery and reattach-before-drop; no CASCADE or replayed charge shortcut.
The pinned all-family acceptance SQL is only a catalog assertion, not a substitute
for concurrent replay, FK/RLS negatives, crash/detach/archive/PITR and restore tests.
Physical operator DDL changes measured catalogs: ordinary migration admission
continues to refuse unreviewed drift. A separate authorized operational authority
snapshot/reconciliation is required; no blanket fingerprint bypass is introduced.
