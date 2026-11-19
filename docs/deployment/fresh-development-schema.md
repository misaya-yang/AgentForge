# Fresh development database

This is the user's explicit new-project engineering mode. It initializes only
`gateway_dev_fresh`, leaving the existing `gateway`, clones, project/volume and
all six existing login passwords intact. Historical upgrade tools and frozen
files remain available for old databases; none of their measured previews,
producer-retirement proofs, activation receipts or N-1 runs is a fresh prerequisite.

Run the single native entry with the existing six distinct principal file/mTLS
connections pointed at `gateway_dev_fresh`:

```text
database-authority fresh-development-init --expected-database gateway_dev_fresh
```

It validates the complete compiled source, creates the database from template0
as the existing NOLOGIN owner through the independent **local Unix socket
operator**, and installs the complete schema in one transaction on that same
independent local peer authority. All platform DDL executes under SET LOCAL ROLE
to the existing unprivileged NOLOGIN owner. PostgreSQL trusted-extension members
are owned by the bootstrap superuser: inside the same transaction only, the local
operator revokes their PUBLIC EXECUTE after checking the exact three-extension
allowlist, pg_depend membership, public schema, function kind, original member
owner and extension owner. It immediately restores the NOLOGIN owner. No process
role is promoted or granted a new privilege. It checks the real final catalog and all five real service
login connections before returning success. A partial schema transaction rolls
back; database creation itself is necessarily outside that transaction. It never
drops or empties an existing database. An unrelated or partial nonempty database is
refused. An already initialized exact fresh database is only reverified; its schema
is not executed again.

The bootstrap process must have the PostgreSQL operator's existing Unix socket/OS
identity; the defaults are socket directory `/var/run/postgresql`, user `postgres`
and port5432. The nonsecret configuration names are
`AI_GATEWAY_FRESH_OPERATOR_SOCKET_DIRECTORY` and `AI_GATEWAY_FRESH_OPERATOR_USER`.
Use the existing PostgreSQL socket mount/operator executor with the matching peer
identity. No TCP superuser, Docker socket, password argv/env, HBA relaxation,
CREATEDB grant to migrator, password rotation or replacement volume is introduced.
The same native process must be permitted to read the six existing private file
credentials; provision the bootstrap identity/mount permissions accordingly.

The six connection interfaces retain the canonical
`AI_GATEWAY_DATABASE_<PRINCIPAL>_DSN_FILE` configuration and existing DATA_MIGRATION,
DATA_GATEWAY, DATA_RUNTIME, DATA_CAPABILITY_WORKER, DATA_KNOWLEDGE and
DATA_KNOWLEDGE_WORKER file/TLS aliases. DSN/password/CA/cert/key remain private
files. Their physical database/expected database metadata must select
`gateway_dev_fresh`, with each original dedicated role. All file-based principals
must use distinct mTLS keys and authenticated client certificates. The existing
explicit local legacy connection interface remains available for development;
file/mTLS and legacy connections cannot be mixed within one admission.

Read-only real revalidation needs no operator connection:

```text
database-authority fresh-development-verify --expected-database gateway_dev_fresh
database-authority fresh-development-contract
```

The second command exports the exact compiled static admission contract and does
not claim a database was checked. Fresh init/verify embed all definition inputs;
they need no source checkout or source-root mount. The dedicated
`fresh-development-schema` binary exposes the same `init`, `verify` and `contract`
commands, plus an explicitly offline `inventory --source-root /source` check.

## Definition and privilege coverage

`fresh_schema/source.json` binds every actual input; `inputs.json` packages those
same bytes for the native image. The installer reuses the complete baseline DDL,
reference rows and grants and all current definitions through035 in their native
dependency order inside the single schema transaction. The order is an internal
construction detail, not a sequence of upgrade/preview/activation prerequisites.
Every original precondition and postcondition executes against PostgreSQL at the
appropriate construction point, with its original statement/lock budgets.

This includes Runtime thread owners, SDK model plane, Knowledge pipeline claims,
Gateway authorization/Cedar audit, A2A and outbound authority, Local Node key
assurance, all earlier service structures and all22 Data families/control routines.
Existing payload tables, fields, indexes, checks, FK actions and RLS expressions
are retained; A2A's new mutable fields have exactly
UPDATE(remote_task_id,cancel_state). The source-owned final ACL policy enumerates
the complete exact1141-line business/authority ACL matrix, plus the separate
exact partition_ops owner/private table/function policy. Once fresh metadata is
installed, exactly15 additional owner/reader ACL records are required: the private
state table, owner-only catalog function and two exact service reader grants.
The full1156-line installed matrix rejects every missing/duplicate/extra record;
only those15 literal records are separated when validating the unchanged1141
business matrix. The new database belongs
to NOLOGIN owner and therefore its explicit database owner policy includes
CONNECT/CREATE/TEMPORARY; no application gains database/schema CREATE.

Installation keeps the original payload layout and registers every Data family;
physical partition expansions/cutovers and enabled feature markers are separate
operations. They are not needed for the fresh service startup. No Batch contract
marker, HA release, external-provider acceptance or destructive retention action
is activated by fresh init.

## Actual final admission

Fresh mode has its own owner-private `public.platform_fresh_schema_state`, containing
the source SHA, actual final fingerprints, supplemental partition catalog and
complete actual catalog snapshot. The three old ledger tables remain empty:
fresh mode does not invent a historical baseline adoption or upgrade receipt.

The owner-installed zero-argument SECURITY DEFINER readers are
`gateway.platform_fresh_schema_snapshot_v1()` for Gateway/Knowledge and
`assistant.platform_fresh_schema_snapshot_v1()` for Runtime/Capability Worker.
Each is EXECUTE-only for its exact service roles and migrator, PUBLIC is revoked,
and search_path is fixed to pg_catalog/pg_temp. The private catalog function and
state table are owner-only. Services do not change roles or read state directly.

The actual catalog covers all28 existing catalog query families plus nested enum,
domain and composite definitions, every field/default/index/check/FK, function
body/security flag, trigger, table/schema/type/column/default ACL, role attributes
and memberships, RLS policy expressions/FORCE flags, extensions, immutable system
reference rows, all22 family definitions and statement policies, and empty legacy
ledgers. Each admission recomputes that real catalog and requires exact equality
to the source-bound initialization snapshot. The initializer/verify additionally
recomputes the four real native fingerprint classes and supplemental partition
catalog; no final fingerprint literal is fabricated in source.

Native application admission selects this mode when the fresh marker exists,
checks the canonical unprivileged login/NOLOGIN reader owner, the exact compiled
fresh source contract and all current service column/function privilege probes.
Missing, denied, malformed or drifted fresh admission fails closed with no legacy
reader fallback. Old database admission is unchanged when the fresh marker is absent.

The static contract is exactly the output of `fresh-development-contract`;
the database reader adds only `fingerprints`, an object with exactly `structural`,
`acl`, `extensions`, `reference_data`, each a real64-character lowercase SHA256.
The static fields must match exactly, including catalog_matches=true,
transactional_initialization=true, historical_upgrade_acceptance=false and the
actual database name. Runtime/Worker use this same contract after their existing
canonical principal identity check, choosing the fresh reader only when the marker
exists. They must never accept arbitrary epochs/hashes or fall back after denial.

## Verification still required

For the separate mutable account initialization, see
[First administrator](fresh-administrator.md). Schema initialization alone does
not create an account or prove that Console login works.

Source tests cover all35 definitions/all22 families, complete catalog query and
permission coverage, exact compiled source reconstruction, malformed/foreign
fresh snapshots, ACL additions/removals/duplicates and wrong database rejection.
They do not establish actual initialization or process acceptance. Controlled
builds and real PostgreSQL execution have separate measured evidence. The opt-in ignored test
`fresh_schema::tests::actual_fresh_catalog_refuses_column_index_constraint_acl_rls_and_function_drift`
requires `AI_PLATFORM_FRESH_SCHEMA_VERIFY=1` and a real initialized dedicated database;
it performs owner DDL in rolled-back transactions and proves real column, index,
constraint, broad ACL, RLS policy and function-security drift is refused, followed
by real read-only service-role verification. Historical upgrade acceptance remains
explicitly false even after fresh-mode acceptance succeeds.

## Bounded native SQL transport

The fresh initializer executes each exact top-level SQL statement on the same
outer transaction, using the authority lexer to keep dollar-quoted routines,
comments and escaped strings intact. All1737 original GRANT/REVOKE statements are
retained; no SQL file is rewritten and no intermediate commit is introduced.
Native SQLx RawSql drains each statement, and a native Sync/ping barrier establishes
readiness before the next phase. The complete251-row baseline verification SELECT
uses RawSql.fetch_all, avoiding the large-batch-to-prepared-query transition.
All original condition checks still run against PostgreSQL at their construction
point; no nested condition transaction is needed inside this single owned transaction.

Fixed redacted phase/sequence diagnostics identify grant execution, baseline
verification, every definition, exact ACL, real catalog/fingerprint capture and
commit. They contain no SQL, DSN, password, file contents or driver Display text;
only a validated five-character SQLSTATE is emitted on database failure.

The complete operator+initialization entry is bounded at150s, migrator init at120s,
read-only reverify at45s, phase/statement I/O at30s (large source scripts at60s),
connect at10s and close at2–3s. Original server statement/lock budgets remain
unchanged. Fresh schema transactions additionally use20s idle-in-transaction timeout
and1s client-disconnect checks. A failed/cancelled operation discards its dedicated
connection with bounded TLS shutdown, causing the owned uncommitted transaction to
roll back; it never enters an unbounded unlock/cleanup query on a broken protocol.

Additional real opt-in regressions are
`actual_mtls_complete_grants_are_drained_before_all_251_followup_checks` (all1737
real grants, native protocol barrier, all251 returned baseline checks, rollback,
then real final admission) and
`actual_cancelled_idle_transaction_discards_backend_and_rolls_back_ddl` (actual
owned DDL, deadline, backend disappearance, DDL absence, final admission). They
require AI_PLATFORM_FRESH_SCHEMA_VERIFY=1 and the initialized dedicated database.
The transport regression does not mistake historical baseline ACL verdicts on a
modern database for fresh acceptance; its temporary grants are rolled back before
rechecking the real final schema.

## Runtime acceptance without compiler credentials

After successful initialization, the same private runtime job can run:

```text
database-authority fresh-development-acceptance --expected-database gateway_dev_fresh
```

This requires actual file/mTLS migrator authentication and checks all six real
connections before and after its probes. Six rollback-only negative transactions
prove rejection of column, index, check, broad outbound UPDATE, RLS policy and
function-security drift. Another rollback-only transaction streams the complete
1737 original grants and receives all251 followup rows; their historical baseline
verdict on a modern schema is not used as fresh acceptance. Finally, both idle
and active client deadlines must remove their owned backend and roll back probe
DDL. The actual final catalog/fingerprints and service-role admission must remain
identical. The command is bounded at120s. No private files enter the compiler role.

## Fresh text-hash executable closure

The frozen legacy import was SECURITY INVOKER even though Gateway lacks the
required table INSERT privileges. Fresh mode makes only the exact source-owned
ensure/import entries owner definers, preserving every function EXECUTE and
table/column ACL. Ensure first locks and checks the authoritative session against
the complete tenant/user/session tuple, then rejects a thread UUID bound to another
scope. Its original source-kind, owner and idempotency checks remain. Import keeps
its original session, in-flight, content and idempotency checks. Both entries and
the existing append definer use the fixed owner-only schema path
`pg_catalog, assistant, pg_temp`; four dynamic relation routes and five helper
calls are explicitly assistant-qualified. No current_schema fallback remains.

The six pgcrypto text SHA256 calls cannot require a broad application or PUBLIC
extension grant. Fresh mode
replaces exactly three pinned source patterns (counts1+4+1) with the equivalent
`pg_catalog.sha256(pg_catalog.convert_to(text,'UTF8'))`, retaining its signature and original application ACL. A non-UTF8 database,
changed original body or extra/missing hash call is refused. Frozen baseline SQL
bytes remain unchanged; the fresh source contract binds the explicit transformation.
The full catalog additionally records every allowlisted extension member's actual
function definition and exact ACL. Platform routines must have no unresolved direct
call to an extension-only function; private extension ACLs remain bootstrap-owner
EXECUTE only, with no new owner/application/PUBLIC grant.

The one already measured d6 engineering database can be repaired by the same
fresh init entry, only through its independent local operator. Before any body or
metadata change it must match that exact source, real complete catalog digest,
four measured fingerprints, partition catalog and full ACL. The fixed transformation,
updated catalog reader and newly measured metadata commit atomically. Every other
predecessor or any drift is refused. This does not adopt an old database or assert
historical upgrade acceptance. It preserves business rows. Runtime acceptance
also executes actual Gateway file/mTLS native-session ensure and Unicode
history/tool import. A separate local-authority transaction seeds Runtime-owned
approvals and owner-only legacy run/queue inputs, returns to the exact Gateway
role for every business call, and exercises terminal receipts and all in-flight
statuses. This is honestly identified as a role-context fixture, alongside the
actual file/mTLS proof. Both check canonical byte hashes and normalized output,
repeat idempotently, reject tenant/user/session scope, UUID collisions, changed
source and direct extension calls, and run while caller-owned temporary relation
shadows exist. Every unrelated principal performs actual denied entry calls over
its own file/mTLS connection. All fixture data rolls back.
