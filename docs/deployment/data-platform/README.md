# Role pools, tenant transactions and data-store mTLS

This package supplies the data-side implementation for U-DATA-11, all 22
U-DATA-03 append families, U-DATA-04 first-stage Knowledge/Assistant policies and the data-store portion of
U-PE-12. It does not activate a schema proposal or change a running database.
Root Compose/Helm, service entrypoints and authority successor admission remain
integration-owner changes. None of these source checks establish runtime acceptance.

## Connection contract and service integration

`ai_platform_service_primitives::data_connection` exports `ConnectionConfig`,
`DatabaseRole`, `ConnectionMode`, `ConnectionPurpose`, `TenantPool`, `TenantScope`,
`with_tenant_scope`, `current_tenant_scope` and `bind_transaction`.

The constructor reads DSN/password/CA/client certificate/private key **files**;
rejects environment DSN/password values, embedded passwords, wrong role/database
aliases, unverified TLS, unbounded pools and unsafe startup options; and redacts
errors/debug output. DSNs contain an explicit role/host/port/database, with no query-string overrides,
and passwords supplied only
through a separate optional password file. Certificate-only frontend/direct
connections use no password. Certificate/key/CA files are supplied explicitly,
not ambient `PG*` credentials. TLS uses `verify-full`, a client certificate and a
bounded SQLx statement cache. `connect()` checks the actual role, physical database,
absence of superuser/BYPASSRLS, and the exact declared role connection limit.
`_REPLICAS` counts every process sharing that role, including worker processes.
Cancellation pools are capped at two connections/process and data budgets reserve
those slots. An unapplied role-limit bootstrap therefore fails admission rather than pretending
that a planned limit already exists.

For a process-owned environment prefix such as `DATA_GATEWAY`, supply:

| Suffix | Meaning |
| --- | --- |
| `_DSN_FILE`, optional `_PASSWORD_FILE` | Private file inputs; embedded DSN passwords are refused |
| `_TLS_CA_FILE`, `_TLS_CERT_FILE`, `_TLS_KEY_FILE` | Purpose-specific trust and identity files |
| `_MODE` | `direct`, `session` or `transaction` |
| `_EXPECTED_HOST` | `postgres`, or frontend `pgbouncer` |
| `_DATABASE_ALIAS` | Actual direct database, or role-specific pool alias |
| `_PHYSICAL_DATABASE` | Database reached behind the alias |
| `_POOL_MAX`, `_REPLICAS` | Default 4 data connections/process (2 for control), 1 declared role-sharing process |
| `_ROLE_CONNECTION_LIMIT` | Default 64; must match the measured PostgreSQL role |
| `_POOLED_BACKEND_LIMIT` | Default 8, matching the PgBouncer per-user cap |
| `_STATEMENT_TIMEOUT_MS` | Default 30000; allowed 100–300000 |

Call `ConnectionConfig::from_environment(prefix, role, purpose, snapshot)` with an
explicit environment snapshot. Roles are the five existing canonical application
identities; a superuser/migrator/admin cannot be substituted. `config.connect()`
returns a new independently checked pool; control and data pools must be separate
instances even when their public parameters agree.

**Current cancellation compatibility:** `PgOperationStorage` retains a backend
PID/start-time identity. Its data pool must remain direct or session-pooled; its
independent cancellation control pool must connect directly to the same physical
PostgreSQL database and role. `ConnectionPurpose::BackendPinned` rejects transaction
pooling and `CancellationControl` rejects all proxies. Do not merely replace its
DSN with a transaction-pooled endpoint. To use transaction pooling there, the
owning operation facade must acquire an explicit transaction **before** observing
backend identity, hold it throughout execution/cancellation settlement, bind tenant
context, and commit/rollback before release. Until that implementation is admitted,
use the `*_session` aliases. This preserves the existing cancellation guarantee.

For converted tenant operations, `TenantPool::begin(&scope)` or
`bind_transaction(&mut sqlx_transaction, &scope, role, timeout)` installs parameterized
`SET LOCAL` equivalents for tenant, role-specific search path, statement timeout,
idle transaction timeout and `row_security=on`. Context binding accepts an explicit
SQLx transaction, not an autocommit connection. SQLx rollback on drop keeps settings
local on ordinary errors and cancellation. There is no session-level tenant SET.

Gateway/Knowledge/Runtime owners must create `TenantScope` only from authenticated
request identity or a verified job/lease owner. `with_tenant_scope(scope, future)`
provides a restoring task-local boundary; `current_tenant_scope()` fails when
missing. Spawned tasks must explicitly capture/rebind it. Bind context in every
transaction, including read-only queries, delayed jobs and worker retries. Global
claim/discovery, schema admission, auth bootstrap and platform-admin operations
need their separately reviewed control paths; an empty tenant is not one.

## PgBouncer topology

[The overlay](../../../deploy/compose/docker-compose.data-security.yml) reuses the
selected project's PostgreSQL image and owned volume. It adds one hardened,
non-root PgBouncer with no published port. An operator must select a verified
PgBouncer >=1.24 image by immutable digest and its UID/GID; no image is downloaded
or guessed by this package. Transaction pooling enables protocol-level prepared
statement tracking (`max_prepared_statements=200`, SQLx cache 128), with bounded
client/role/database pools, no wildcard aliases, forced backend user or auth query.
Per-role `*_session` aliases retain compatibility during conversion.

Each pool key retains the actual PostgreSQL login role. Certificate CN binds each
frontend client to that role. Backend connections authenticate the same role using
SCRAM and the pooler's separate client certificate. The auth file contains existing
role passwords in a private file, because certificate frontend authentication cannot
supply SCRAM passthrough keys. A server-side SCRAM verifier alone is insufficient
for this topology; do not silently substitute it for the required backend secret.
The tool never changes PostgreSQL passwords.

The dedicated backend DNS alias is `postgres-backend`, with a matching server SAN.
This avoids Docker selecting the client-network address for a pooler backend
connection and accidentally taking the direct client-certificate HBA rule.
`data-postgres` and `data-pgbouncer-backend` require explicit, nonoverlapping private
CIDRs. Inspect the existing project's actual network before selecting them; do not
recreate an active network or volume to satisfy the example. Only PostgreSQL and
PgBouncer join the backend network. Existing application consumers join the client
network; the edge joins neither.

The server requires TLS 1.3; PgBouncer verifies both client and server certificates.
Direct role certificates use PostgreSQL `cert` auth (CN equals role). Pooler backend
connections use SCRAM plus CA-verified client certificates on the dedicated backend
network. Plain TCP, unknown networks, replication and TCP superuser access are
rejected. Administrative recovery stays an explicitly authorized local/authority
operation. The overlay does not change the PostgreSQL major version.

## Private material and file-secrets rollout

Existing private local env credentials can be imported directly with the explicit
[`prepare-local-env` entry point](local-env-import.md), without sourcing the env
file, rotating passwords or contacting the database.

Build only through the shared native container workflow, then use the resulting
controlled operational binary:

```sh
scripts/new/native-container.sh build -p ai-platform-tools --bin platform-data-security
platform-data-security validate --repo-root /repo
platform-data-security prepare --repo-root /repo --directory /private/new-data-release \
  --database gateway --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24 \
  --password-files /private/existing-role-password-paths.json \
  --bootstrap-password-file /private/existing-postgres-bootstrap-password \
  --openssl /verified/path/to/openssl
platform-data-security validate-material --directory /private/new-data-release
```

The CIDRs above are documentation examples, not permission to replace a network.
The password-path map has exactly the five canonical application role names and
private file paths as values. Inputs must be the actual existing role credentials.
Preparation requires a new absolute directory outside the repository. It issues a
small internal CA, purpose-specific server/client identities (including the direct
migrator), a 30-day leaf/365-day CA lifetime, role DSN files, an auth file, HBA and
PgBouncer config. OpenSSL runs directly without a shell; passwords/private keys
are never command-line arguments or printed. Failed preparation leaves an
`INCOMPLETE` marker and cannot validate. Existing files are never replaced, no
ownership is changed, and no DB/service operation runs. Completion receipts bind
public certificate and CA fingerprints, not credentials.

The issuer directory contains the offline CA private key. **Never mount it into a
service**. Mount only the selected service/role directory read-only. Review/install
files under the exact service UID/GID before deployment; private modes are
0400/0440/0600/0640 with no other access. The integrity checker does not claim to
prove a container UID can read its mounts. Kubernetes projected secrets should be
mounted as regular `subPath` files or resolved into a trusted owned private volume;
final symlinks are rejected. On rotation, restart/reload the consumers explicitly
rather than assuming subPath updates propagate. Use External Secrets/Vault/KMS for
Kubernetes sources; keep key values out of Helm values and environment variables.

The overlay removes container `POSTGRES_PASSWORD` and supplies
`POSTGRES_PASSWORD_FILE`. The canonical complete security entrypoint removes
legacy application DSNs and selects scoped file/mTLS connections. The existing
base still requires its original host-side interpolation inputs, even when their
values are discarded from the effective service environment. Bind only the
generated auth file and selected certificate directories into PgBouncer/PostgreSQL.
Application roles mount their own certificate/key/CA and selected DSN file, never
the pooler's auth file.
The health `pg_isready` inherited from PostgreSQL is liveness only, not proof of
mTLS/authentication. Use the explicit per-role `probe` in the owned network:

```sh
platform-data-security probe --role gateway --tenant verified-tenant-id \
  --environment-prefix DATA_GATEWAY
```

Probe opens only the selected role's connection, verifies actual role/database/limit,
binds and reads transaction-local tenant state, then rolls back/closes. It does not
create data or certify cross-tenant RLS/replica capacity. The native authority must
measure the [admin role-limit proposal](../../design/proposals/data-platform/role-pool-limits.admin.sql)
through its admin bootstrap path; a migrator without CREATEROLE must not retry as
an application identity or bypass privileges.

Rotation has a dual-acceptance window: issue a new release, prepare a public
old+new CA bundle with `rotation-bundle --old-ca ... --new-ca ... --output NEWFILE`,
approve/bind that bundle's fingerprint to the deployment release, install trust at
both endpoints, switch role leaves/server leaves, verify every role, then remove
old trust and demonstrate old/wrong-CA/CN clients are refused. Retain the old release
for the measured rollback window. Do not retain old trust indefinitely or call
bundle preparation a rotation drill; no reload or live rejection test is performed
by this tool. Certificate compromise requires the explicit revocation/CA-removal
procedure, not ordinary overlapping renewal.

## Aggregate connection budgets

`ConnectionConfig` preserves the existing per-pool check. The shared
`data_connection::pooling::Budget` additionally accounts for **all pools sharing
one PostgreSQL role**, direct cancellation pools, and each PgBouncer replica.
A role's session and transaction aliases share its pooler's `max_user_connections`;
that cap is not shared across separate pooler replicas. Frontend capacity assumes
all declared clients can fail over to one surviving pooler. Both the per-role
limit and PostgreSQL's total `max_connections`, administrative reserve and all
unmanaged consumers must fit. Duplicate/unknown clients, unsafe cancellation or
transaction modes, wrong aliases, insufficient prepared-statement tracking and
oversubscribed frontends fail before any network operation.

```sh
platform-data-security pool-budget --repo-root /repo \
  --budget /operator/reviewed-complete-role-pool-budget.json
```

Start from [the example](../../../deploy/data/pgbouncer/connection-budget.example.json).
It is a **declared plan** for two app processes per role and one pooler, not a
measured topology. List each independent data/control pool once with the actual
number of processes, include every pooler replica, and reserve all other clients.
The CLI checks the reviewed role limit 64 and current pooler caps 8/256/200; a
future cap change requires a separately reviewed configuration and budget update.
The output always has `replica_capacity_acceptance:false`. `SHOW POOLS`/role counts,
prepared-query reuse, failover and queue/cancellation latency must still be measured
on the selected isolated deployment; a sum of configured values is not that test.

## RLS authority proposal and transaction identity

The independent unnumbered
[complete first-stage proposal](../../design/proposals/data-platform/tenant-rls-complete.sql)
covers **all 48 baseline Knowledge/Assistant tables with a direct tenant column**,
plus dataset-derived `knowledge.documents` and `knowledge.segments` (50 total).
It supersedes the unactivated nine-table candidate; do not install both or edit
an installed/frozen successor. Gateway expansion remains a later measured phase.
Each target keeps its existing grants, enables/FORCEs RLS and has permissive
same-tenant access plus a restrictive same-tenant guard, with matching WITH CHECK.
The owner maintenance role remains explicit and no application may inherit it.
NULL/empty tenant rows remain invisible; never assign their owner from the caller.
Existing scoped owner/fence functions and application resource ACLs remain required.

The transaction binder now rejects a different authenticated task tenant before
acquiring/executing SQL. Its SQL gate checks the actual login role and refuses
replacing a nonempty transaction tenant, while permitting idempotent same-tenant
binding. Missing request scope remains valid only for an explicitly verified
background owner passed by the existing caller. All settings remain SET LOCAL
via parameterized set_config and rollback-on-drop; no session-level tenant SET.
No business API, global discovery/control path or schema activation is introduced.

Use the read-only [catalog observation SQL](../../design/proposals/data-platform/tenant-rls-catalog.sql)
through the authority's authorized observation path, then:

```sh
platform-data-security rls-catalog --catalog /operator/rls-owner-catalog.json
```

The structural controller checks all 50 targets, exact policy identities/roles,
ENABLE/FORCE, non-bypass app roles, owner membership and exact approved predicate
tokens (including both USING and WITH CHECK). Extra OR/true conditions, foreign
parent selectors and a permissive guard fail. It does not issue a new authority
fingerprint or establish actual SQL isolation/plan acceptance. Raw catalog output
is operator evidence; a user-provided JSON alone is not database admission.

The ignored real tests retain the existing missing/empty/foreign read and write
negatives, and now include same-transaction tenant/role rebind refusal, commit,
rollback and cancelled-query connection reuse, dataset-derived document reparenting,
and 16 concurrent role frontend pools bounded by the actual eight backend cap.
They require separately authorized `af_data_*` fixtures; none runs in the ordinary
network-disabled compiler. All app/job paths must be scoped and discovery/control
operations separately admitted before enabling the full first-stage policy.
Actual-login SQL positives/negatives and measured EXPLAIN regressions remain the
activation gate; source and catalog checks do not replace them.

## Partition preparation and retention

The [complete append runbook](append-partitions.md) owns all 22 usage, audit, trace,
lease and model-call families and the installed authority's actual resumable
expand/backfill/validate/FK-rebind/index/cutover/seal/detach/archive/drop functions.
Global typed dedup keys, original FK actions, owner/fence/unknown-effect barriers,
retained identity outcomes, exact original native upserts and tenant binds remain.
The previous audit-only scripts/`retention-plan` CLI remain historical compatibility
inputs; new admission uses the all-family successor, never both designs.

The offline `platform-append-partitions maintenance` controller checks the complete
22-family snapshot inventory, one authority observation and explicit UTC time. It
plans bounded day/month preparation only for already active families. Inactive
families remain explicit blockers, not silently omitted coverage. Schedule owner
observations and execute each selected plan via the existing native authority;
there is no application CREATE grant or autonomous SQL executor. Retention resume
also requires an active family and a catalog-observed detach state; missing ack
never means assume success or guess FINALIZE. Sealing requires a real relation OID.

```sh
platform-append-partitions maintenance --snapshots /operator/all-family-snapshots.json \
  --now 2026-10-18T00:00:00Z --daily-ahead 7 --monthly-ahead 2
```

No database, copy, schema admission, retention operation, PITR/restore drill or
capacity run is performed by these offline commands. Root must allocate the
restored-copy authority/role/RLS/retention/EXPLAIN/capacity acceptance window.

## Verification handoff

The ordinary source batch is:

```sh
scripts/new/native-container.sh test -p ai-platform-service-primitives --features sqlx/chrono --lib -- data_connection --test-threads=1
scripts/new/native-container.sh test -p ai-platform-tools --lib data_security -- --test-threads=1
scripts/new/native-container.sh test -p ai-platform-tools --test data_security_cli --test append_partition_cli
```

The ignored real test is
`data_connection::real_tests::real_rls_and_transaction_pool_scope_do_not_leak_between_tenants`.
It requires `DATA_ACCEPTANCE_AUTHORIZED=isolated-copy-only`, a purpose-specific
`DATA_ACCEPTANCE_*` file connection configuration, an `af_data_*` physical database
with authority-admitted policies, and two pre-seeded tenant/dataset fixtures
(`DATA_ACCEPTANCE_TENANT_A/B`, `DATA_ACCEPTANCE_DATASET_A/B`). It performs positive
and negative application-role DML inside rolled-back transactions, then checks
connection reuse after rollback/commit. Run the compiled test binary only in the
separately owned isolated acceptance environment with those private file mounts.
The normal native build wrapper intentionally has `network=none` and no runtime
credentials: do not weaken that build boundary to run live acceptance.

The complete U-DATA-03 successor now has a separate owning runbook:
[all-family append partitions](append-partitions.md). Its 22-family implementation
supersedes the earlier audit-only candidate; admit the new independent proposal
through the authority rather than activating both designs. Shared writer/authority
integration and unrun live acceptance are listed in that runbook.


The complete service entrypoint is now
[`deploy/compose/docker-compose.security.yml`](../../../deploy/compose/docker-compose.security.yml),
with the optional pooler in `docker-compose.data-session-pooling.yml`. The data
fragment remains the owning PostgreSQL/mTLS contract; the canonical combined
override is freshness-checked with it. Do not merge separate reset fragments
after local `extends`: Compose can restore discarded credentials. Use the
full local base and staged selection documented in
[Native service trust deployment](../../../deploy/workload-identity/DEPLOYMENT.md).
The cert-only TCP profile retains a read-only native `deployment-file-admission`
bootstrap gate on already prepared roles; it does not attempt TCP superuser
bootstrap through an HBA that intentionally rejects superusers.

Additional ignored isolated tests are
`real_dataset_join_policy_rejects_cross_tenant_document_reparenting` (also needs
`DATA_ACCEPTANCE_DOCUMENT_A/B`) and
`real_transaction_pool_replica_connections_and_prepared_statements_stay_bounded`
(transaction mode, reviewed backend cap eight and no unrelated clients sharing
that fixture role). They never create a database or deployment. The original
SQL-layer fixture remains unchanged in authority and rolls back test DML.
