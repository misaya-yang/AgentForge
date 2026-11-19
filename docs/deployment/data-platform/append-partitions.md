# Complete append-family partition successor (U-DATA-03)

This independent unnumbered proposal replaces the earlier **audit-only candidate**.
It implements the entire planned append set and current usage/audit/model-call
extensions. It does not activate schema, edit successors <=029, change frozen
fingerprints, connect to a database or establish retention/PITR acceptance.

The package has four authority inputs, in order:
`docs/design/proposals/data-platform/append-partitions/{expand,migrate,write-compatibility,retention}.sql`.
The authority must review/admit independent successors and measured fingerprints;
never append these definitions to an already frozen migration. Do not apply both
the old audit-only expansion and this fresh family expansion. An already activated
older design needs its own measured conversion successor.

## Coverage

| Family | Clock | Cadence |
| --- | --- | --- |
| gateway.usage_records | created_at | month |
| gateway.request_traces | created_at | day |
| gateway.audit_logs | created_at | month |
| gateway.billing_events | event_time | month |
| gateway.agent_trace_spans | started_at | day |
| gateway.agent_trace_events | occurred_at | day |
| gateway.tool_audit_log | created_at | month |
| gateway.login_audit | created_at | month |
| gateway.service_health_records | checked_at | month |
| assistant.assistant_runtime_items | created_at | day |
| assistant.assistant_audit_events | created_at | month |
| assistant.assistant_capability_events | created_at | day |
| knowledge.kb_document_progress_events | created_at | month |
| assistant.assistant_runtime_model_calls | reserved_at | month |
| assistant.assistant_runtime_model_leases | issued_at | month |
| gateway.knowledge_model_calls | issued_at | month |
| gateway.task_reconciliation_audit | resolved_at | month |
| gateway.price_book_v2_activation_audit | created_at | month |
| gateway.price_book_v2_import_audit | created_at | month |
| gateway.price_book_v2_usage_receipts | created_at | month |
| gateway.price_book_v2_accounting_checkpoints | updated_at | month |
| gateway.agent_traces | started_at | day |

These 22 entries agree in SQL, native catalog and shared append interface. Missing
026/027/029 prerequisites block that family's admission; no tool silently narrows
the work to audit_logs. `singleton_worker_leases` is a bounded mutable coordination
record, not an append history; it keeps its existing lease/fencing contract.
Security replay nonces retain their separate expiry/key-rotation policy.

## Global keys and relational compatibility

A partitioned payload parent keeps the original column types, generated values,
defaults, CHECKs, outgoing FKs, triggers, ACL and RLS policies. The twenty NOT NULL
clocks partition directly by their original clock column. The two legacy nullable
clocks use `COALESCE(clock, '-infinity'::timestamptz)` as an expression, leaving
nullable source timestamps unchanged. No ingestion timestamp or fabricated date is written into an
existing fact. The attached MINVALUE legacy partition receives NULL-time facts;
such a partition cannot be retained/dropped until unknown-time facts are resolved
through an explicit authority/data-governance process. Future normal partitions
have exact UTC month/day bounds. Ordinary bounds prune the direct NOT NULL clock
keys. Nullable-clock time-window queries use the native adapter's additional
matching COALESCE expression, keeping the original predicate and timezone. Owners
must measure their actual role/RLS EXPLAIN plans before activation. After a
historical range is retired, NULL/backdated writes into that retired range fail
closed until an explicitly admitted destination exists. The tool never fabricates
a date or silently creates an empty replacement as though it restored old facts. There is no DEFAULT partition, avoiding its
incompatibility with concurrent detach.

Each family has a private, nonpartitioned **typed key directory**. It has the same
SQL column types but stores only columns needed by original global unique indexes
and scope. Other original columns are NULL; no request/result/trace payload is
copied into the directory. The directory reproduces every original unique index's
access method/key expressions, partial predicate, collation/opclass and NULLS NOT
DISTINCT semantics. Its extra JSON primary identity records the original typed PK.
For example, usage's exact tenant/request/service/type expression and partial
nonempty-request predicate remain globally unique across every time partition.
It is never replaced by a `(key, created_at)` unique index.

Plain INSERT/UPDATE/DELETE is guarded by the directory trigger. UPDATE synchronizes
scope/unique keys and time; PostgreSQL row movement uses a transaction-scoped move
marker, so moving a row between partitions does not accidentally fire DELETE FK
actions or count as a new append. Generated unique-key evolution not present in
the admitted source requires an explicit future successor; it never gets ignored.

Incoming FKs move to the directory with their **original columns, MATCH, ON DELETE,
ON UPDATE and deferrability**. This includes event/score/eval/self references to
spans and the full six-column runtime lease scope. A reference trigger additionally
requires the referenced payload partition to remain active. It holds a state-row
lock through commit, closing new-reference races with retention. Ordinary explicit
payload DELETE removes its directory row and fires the original FK actions. A
partition DROP retains keys as idempotency tombstones; it is allowed only after
all external inbound payload references have drained. Future references to those
tombstones are rejected, even though the global key remains reserved.

Directories and new leaves explicitly revoke inherited default ACL grants. App
access stays on the original parent contract. The root owner must not grant app
access to private directories or maintenance functions. App writes still execute
as the invoker on the real table: original privilege checks and FORCE RLS remain
in effect. There is no new general security-definer app write API.

## Native upserts and plain writers

PostgreSQL cannot infer an old global `ON CONFLICT` target on a range parent that
has only a private global directory. The native Foundation/Gateway/GatewayEval upsert sites now pass through
`data_connection::append_store` **in their existing accounting/admission
transaction**, selected by the family's authority state. Its `statement` mode
uses exact original conflict-tail policies from `native-writer-policies.sql`.
The direct `partition_ops.write_json(family,row,mode,original_index_name)` API also
remains available to checked callers with its optional statement parameters. The entrypoint materializes typed defaults
once, preserves generated columns, and supports:

- `insert`: original uniqueness failure; no retry.
- `ignore`: original conflict-any DO NOTHING, returning only inserted=false.
- `same_or_error`: select the exact original global conflict index, retain the
  existing row if supplied typed fields agree, otherwise raise a body conflict.
- `usage_merge`: only `idx_usage_records_request_identity`, preserving the
  existing RAW_CONFLICT maxima, terminal-status rule, first error, metadata merge,
  immutable pricing snapshot and provider/model/price guard.

The result is `{inserted,outcome,row}`. A payload removed by retention returns
`retained_identity` with no row to direct modes; native statement UPDATE mode
raises inside the writer so an outer SELECT cannot filter the error away. This
**never authorizes a new charge, quota stage or admission**. The durable price-book checkpoints/receipt contract decides how
an old request is reported. `pricing_conflict` also cannot authorize accounting.
Existing stored owner functions using plain INSERT keep the directory trigger;
no blanket new INSERT privilege is granted to their callers.

The invoker writer calls metadata-only `write_spec`, `statement_spec` and
`lookup_conflict`. The independent `legacy-retention-adapters.sql` proposal grants
schema USAGE and only the listed metadata/invoker entrypoints to precise existing
native roles, retaining their original table/column grants. `lookup_conflict` requires the
transaction's bound tenant and does not expose another tenant's global identity.

The shared Rust adapter is
`platform-service-primitives::data_connection::append::{execute,AppendMode,AppendOutcome}`.
It verifies a `TenantTransaction`, has no commit/retry, and returns a distinct
retained-identity outcome. Owners using another transaction facade must bind the
same verified tenant and execute the same parameterized SQL within that existing
transaction. Do not open another transaction just for the append write.

Mandatory owner handoff:

- Gateway/Foundation: usage_recorder RAW_CONFLICT, billing event DO NOTHING,
  provider-batch usage/receipt/checkpoint upserts; retain existing exactly-once
  accounting checkpoints and transaction ordering.
- Runtime/Execution: runtime item event ID/event key/sequence, capability event
  identities, runtime model call lease+hash and lease/snapshot scope upserts.
- Knowledge/model-plane owner: progress events and any upsert in owner functions;
  retain existing signed model-call identity, quota serialization and unknown-state
  reconciliation. Do not partition away active/unknown model operations.
- Authority: dependency inventory/rebind for views, prepared statements, row-type
  function contracts and grants. Existing objects bound to the original heap OID
  must not remain a legacy-only read/write path. Drain/reprepare affected client
  pools as part of the reviewed cutover; source renaming alone is not evidence.

## Resumable migration and maintenance

Native `platform-append-partitions` is an offline authority plan exporter; it accepts
no DSN and has no direct execute/bypass mode. SQL operations are actually implemented
by the installed owner-only functions, and the existing native authority executes
the reviewed plan. All mutation calls require the canonical NOLOGIN schema owner.
The authority execution wrapper must enforce emitted lock_timeout=2s,
statement_timeout=60s, UTC, the exact current snapshot and a fresh observation after
every statement. CONCURRENTLY commands must run outside transaction blocks and
must reset their connection settings afterwards.

1. `expand`: creates the empty parent, exact global indexes, source triggers and
   NOT VALID identity/bound constraints. Its admission digest and chosen future
   UTC boundary are immutable resume scope.
2. `backfill`: <=10,000 rows per committed call, typed PK keyset order, source row
   locks, persisted cursor/revision and directory/source consistency checks. An
   empty final batch marks completion. No OFFSET or unbounded transaction.
3. `validate_legacy`: online FK and bound validation, separate from cutover.
4. `capture_foreign_keys` and one-FK `rebind_fk(expand/validate/contract)` preserve
   every original FK. A partitioned referencing parent cannot use NOT VALID in
   PostgreSQL; its necessary validation is an explicit bounded phase, not hidden
   under cutover. Process target dependencies before enabling parent cutovers.
5. `legacy_index_sql`: returns required matching legacy CREATE INDEX CONCURRENTLY
   statements, including BRIN and nonunique companions for former global unique
   indexes. Existing INVALID/incompatible artifacts cause a diagnostic refusal;
   authority must inspect and explicitly recover them. Cutover refuses missing
   valid indexes, preventing ATTACH from building them under its exclusive lock.
6. `prepare`: <=24 months or <=90 days ahead, exact catalog/OID/bounds registration;
   no blind IF NOT EXISTS reuse. Root schedules this via controlled authority
   maintenance, not by granting the Gateway app schema CREATE.
7. `cutover`: requires checksum-verified PITR, role/RLS, N-1 app compatibility,
   dependency-rebind and authority approval artifacts. It briefly locks source,
   preserves triggers/ACL/RLS, renames/attaches the prevalidated heap, transfers
   sequence ownership and records compatibility digest. It does not scan/backfill
   rows or build missing legacy indexes under the cutover lock.

Native commands:

```text
platform-append-partitions catalog
platform-append-partitions snapshot --catalog OWNER_STATUS_JSON --authority-snapshot AUTHORITY_RECEIPT_JSON
platform-append-partitions plan --snapshot WRAPPED_SNAPSHOT_JSON --operation OPERATION_JSON --now UTC_OBSERVATION
platform-append-partitions verify-archive --manifest ARCHIVE_MANIFEST_JSON --receipt ARCHIVE_RECEIPT_JSON
```

`Operation` JSON is a tagged Rust enum in `append_partitions/mod.rs`; schema fields
are denied when unknown. `snapshot` computes checksums itself. A catalog checksum
proves local byte consistency, not database authority or live acceptance; retain
the actual owner/authority observation and approval. Evidence paths are explicit
absolute regular files, checked against supplied SHA-256. No credential is read.

## Retention and recovery

Retention seals one closed partition under a per-partition state barrier, draining
concurrent writes and new-reference transactions. INSERT, UPDATE and DELETE into a
sealed partition fail. Sealing also saves and revokes the leaf's table/column ACL
grants: PostgreSQL can remove inherited triggers during DETACH, so a previously
granted legacy leaf must not become directly writable after detach. Reattach
restores the exact saved grants atomically and invalidates stale archive readiness
while retaining its history. Predicates reject running traces, active threads/executions,
active or unaccounted leases, unresolved/unknown calls and active ingestion. NULL
time is never treated as an expired known fact. Counts and source snapshot are
recorded at sealing. External FK references, including nonpartitioned consumers
and detached tables, block detachment; retire referencing child payload first.

The next owner catalog selects DETACH CONCURRENTLY, pending-detach FINALIZE or
state synchronization. A lost acknowledgement never means assume success or guess
FINALIZE. A verified detached archive receipt binds family, relation name/OID,
sealed row count, original source snapshot, archive bytes SHA-256, manifest SHA-256
and restore-validation artifact SHA-256. Native verification streams archive bytes
and compares manifest metadata; it does not claim an actual restore drill ran.
Only an exact recorded receipt permits DROP, without CASCADE or large payload
DELETEs. Minimal key directories remain; their long-term size is a separate
idempotency/PII governance cost, not hidden payload duplication.

Before DROP, `reattach` restores the preserved partition under the exact old bounds
and reopens the state barrier. After DROP, recovery requires restoring the verified
archive/PITR through the authority. No tool silently re-creates an empty partition
or clears global dedup keys. Changing archive evidence after approval is refused.

## Verification and remaining gates

Added native unit coverage: all-family catalog, calendar/leap/month boundaries,
checksum/phase refusal, bounded backfill, catalog-driven detach resume, evidence
hash validation, exact archive/drop requirements, tenant request validation,
original usage partial-key semantics and tombstone accounting behaviour. A native
CLI test rejects DSNs/execute mode. An ignored real test requires explicit restored
copy authorization, file-secret mTLS, a database named `af_partition_acceptance_*`
and two prepared periods, and rolls back all cross-period usage/audit fixtures.
`acceptance-catalog.sql` checks global index/RLS/FK/range/BRIN metadata for all 22
active families in an explicitly marked restored copy.

The source and native-check results are recorded separately in the current owning
external data-service evidence; they do not establish SQL rehearsal. All real
restored-copy SQL/role/retention tests remain explicit isolated acceptance gates.
Real PITR/RPO/RTO, all-role and SQL-layer tenant negatives, exact FK actions under
concurrency, interrupted migration/detach, application N-1, partition pruning and
query-plan regressions, archive restore and role deployment remain mandatory
acceptance. Source completion does not open the singleton/HA/release gate.

Business wiring and exact activation/retention gates: [append-business-wiring.md](append-business-wiring.md).

The directory reserves new identities in its first AFTER ROW INSERT trigger,
after the original RLS WITH CHECK, NOT NULL and CHECK constraints. Its BEFORE
trigger holds the retention barrier. The AFTER trigger runs before FK constraint
triggers so self references still see the new directory identity. A duplicate
cannot be caught before original INSERT checks and thereby skip those checks.

Scheduled preparation uses `platform-append-partitions maintenance --snapshots
ALL_22_SNAPSHOTS --now UTC_TIME --daily-ahead 7 --monthly-ahead 2`. The array must
cover all 22 families from one authority observation; duplicate/missing/mixed
families are refused. Only active families receive prepare plans. Inactive families
remain named blockers, and no cutover/retention approval is manufactured. Root
schedules fresh owner catalog observations and executes each bounded plan through
native authority, checking revisions and refreshing the snapshot after each step.

The current authority wrapper for these unchanged SQL inputs is the source-owned
`035_complete_append_partition_authority.sql` successor. This data closeout does
not regenerate it, change frozen authority/fresh-schema inputs, or admit a live
cutover. The complete RLS first stage is a separate new unnumbered proposal;
root must admit it independently after its actual-role/plan rehearsal.
