# 07 Data platform (database authority, PostgreSQL, Valkey/Redis, Qdrant, object storage)

status: proposed · domain_id: platform-upgrade-2026-10 · last_verified: 2026-10-16 (HEAD `22d9ce0`)

## 1. Role

PostgreSQL is the system of record for all three contexts. Redis (Valkey after U-DATA-05) is
ephemeral coordination and acceleration. Qdrant holds derived vector indexes. Object storage holds
blobs. The native migration authority is the only writer of schema, and every database transition
goes through it and its frozen verification (AGENTS.md).

## 2. Current architecture (SRC)

| Component | Current state |
| --- | --- |
| PostgreSQL | `postgres:16-alpine`, one database, schemas `gateway` (82 tables), `assistant` (48), `knowledge` (33). Extensions `uuid-ossp`, `pgcrypto`, `pg_trgm`. No partitioned tables and no row-level security in the baseline (F-21). |
| Migration authority | `platform-migration-authority` (19.8k lines). A frozen baseline (`2026_08_post_kb_v1`: `init.sql`, `grants.sql`, `verify.sql`, ownership and grants policies, data-access inventory), ledger tables, an advisory lock, raw fingerprint verification, epochs (17 current), and a successor chain (021–023) staged as proposals ([authority README](../../../database/authority/README.md), [successor chain](../../design/canonical-schema-successor-chain.md)). |
| Roles | `ai_gateway_owner` (NOLOGIN), `_migrator`, `_gateway`, `_runtime`, `_capability_worker`, `_knowledge_api`, `_knowledge_worker` exist ([roles.sql](../../../database/bootstrap/roles.sql)). Compose still connects every service, and the migrator by default, as the bootstrap superuser, with a `search_path` that spans all three schemas (F-05). |
| Redis | `redis:7-alpine`, one instance, AOF on, `allkeys-lru`, 128 MB. It holds session allowlists, capacity leases, rate windows, usage streams and caches (F-40). |
| Qdrant | `qdrant/qdrant:v1.18.2`, single node, no API key, reachable from every container on the one bridge network (F-39) |
| Object storage | `local` by default (`gateway-data`, `kb-data` volumes). An S3-compatible backend is configurable for the Gateway and Knowledge ([docker-compose.yml:343–349, 501–507](../../../docker-compose.yml)); the local profile runs RustFS 1.0.0, pinned by digest ([deploy/local/compose.yml:41](../../../deploy/local/compose.yml)). |
| Backup | `make backup`: a gzipped `pg_dump` of PostgreSQL only (F-38) |

Append-heavy tables that will dominate size: `gateway.usage_records`, `request_traces`,
`audit_logs`, `billing_events`, `agent_trace_spans`, `agent_trace_events`, `tool_audit_log`,
`login_audit`, `service_health_records`; `assistant.assistant_runtime_items`,
`assistant_audit_events`, `assistant_capability_events`;
`knowledge.kb_document_progress_events`.

## 3. Assessment

Strengths to keep: a single schema authority with fingerprints, ledger, advisory lock and
fail-closed verification; least-privilege roles already designed with a data-access inventory;
explicit rules against bypassing the authority; secrets kept out of Redis process arguments.

Gaps, in order of consequence:

1. **Recovery (F-38).** No point-in-time recovery, no vector or blob backups, unencrypted dumps.
   A lost Qdrant volume means re-embedding every dataset.
2. **Isolation (F-05, F-39).** Superuser connections make the designed roles moot. Qdrant is open
   to every container.
3. **Coordination safety (F-40).** Eviction can drop admission state silently. Correctness data and
   cache data need different memory policies.
4. **Growth.** Unpartitioned append-only tables make retention a `DELETE` workload (bloat, vacuum
   pressure), and time-range queries scan large heaps.
5. **Scale-out blockers.** Local blob volumes pin the Gateway and Knowledge to one host; there is
   no connection pooler for many replicas.
6. **Versions.** PostgreSQL 16 is two majors behind 18; Redis 7 should become Valkey 9 (P-ADR-11);
   Qdrant 1.18 lacks the 1.19 features that 04 depends on.

## 4. Target architecture

### 4.1 PostgreSQL 18, then 19 (U-DATA-01, U-DATA-01b)

- **18 in Ph1.** Rehearse on a *copy* of a development volume, never the owned volumes. Then run
  the authority's `verify` against the upgraded copy: catalog-level fingerprints (ACL text,
  defaults) must be proven, not assumed, across majors. Use `pg_upgrade --link` with a measured
  downtime window; PostgreSQL 18 keeps planner statistics across `pg_upgrade`.
  - Data checksums: `initdb` in 18 enables them by default, and `pg_upgrade` requires matching
    settings. Either initialize the new cluster with `--no-data-checksums` and enable checksums
    later with `pg_checksums` in a maintenance window, or upgrade logically. Decide this in the
    rehearsal.
  - Adopt what pays off: asynchronous I/O (`io_method`; measure `worker` against `io_uring`),
    `uuidv7()` for database-generated keys, B-tree skip scan, virtual generated columns for derived
    fields, and `RETURNING OLD/NEW` in audit writes.
- **19 in Ph3**, after GA (scheduled 2026-11-19) and its first minor release (WEB). Benefits:
  `REPACK … CONCURRENTLY` for bloat without an extension, parallel index vacuuming, logical
  replication of sequences (simplifies blue/green major upgrades), and `WAIT` for read-your-writes
  on standbys. Treat SQL/PGQ as unavailable; reports say it was pulled from the release.
- **Observability.** `pg_stat_statements` and `auto_explain` (sampled) in every profile; I/O
  statistics feed the OTel Collector (U-PE-11).

### 4.2 Least privilege everywhere (U-DATA-02)

- Each service connects as its own role, with its own `search_path` limited to the schemas its
  data-access inventory grants, a per-role `CONNECTION LIMIT`, and per-role `statement_timeout`
  and `idle_in_transaction_session_timeout`.
- The migrator uses its own DSN. The superuser is used only for bootstrap and the explicit admin
  repair commands, and is absent from every long-running service's configuration.
- CI fails if any service configuration in Compose or Helm names the superuser.

### 4.3 Partitioning and retention (U-DATA-03)

- Declarative range partitioning by time (monthly; daily for the hottest trace tables), with BRIN
  indexes on the time key and partitions created ahead by a scheduled job (`pg_partman` or the
  Gateway worker role).
- Retention becomes `DETACH … CONCURRENTLY` plus drop, or archive the partition to Parquet in
  object storage first. That replaces large `DELETE`s.
- Migrate online: create the partitioned parent, attach the old table as the first partition with
  a pre-validated `CHECK` constraint, and route new writes to new partitions. Ship each table as an
  authority successor with a rehearsal on a restored snapshot.

### 4.4 Row-level security as defence in depth (U-DATA-04)

- Application code keeps enforcing tenant scope. RLS adds a second, database-enforced layer:
  `tenant_id = current_setting('app.tenant_id')::…`, set with `SET LOCAL` in each transaction, with
  `FORCE ROW LEVEL SECURITY` for the application roles.
- Start with `knowledge` and `assistant` tenant tables. Measure plan regressions, then extend to
  `gateway`. Platform-administration paths use a separate role with explicit `BYPASSRLS`, audited.
- Align with ADR-010 (tenant identity and knowledge authorization).

### 4.5 Valkey 9: split coordination from cache (U-DATA-05, P-ADR-11)

| Instance | Holds | Memory policy | Durability |
| --- | --- | --- | --- |
| `coord` | Session allowlists, capacity leases, token reservations, rate windows, idempotency keys | `noeviction`: writes fail visibly and admission fails closed | AOF `everysec`; alert at 70 % memory |
| `cache` | Principal and policy caches, retrieval caches, transient streams | `allkeys-lru` | None needed |

- Valkey 9 (BSD-3) instead of Redis 8 (whose open-source option is AGPLv3) (WEB). Per-service ACL
  users instead of one shared password, and TLS.
- Keys carry `{tenant}` hash tags so the same scripts work on Valkey Cluster. HA through Sentinel
  or Cluster in Kubernetes.
- Usage streams stop being a durability buffer: billing durability moves to the PostgreSQL outbox
  (U-GW-07, 02 §4.4).

### 4.6 Qdrant (U-DATA-07, U-DATA-08)

- **1.19.2 in Ph1** (U-DATA-07). Snapshot before the upgrade and follow Qdrant's one-minor-at-a-time
  upgrade path. The 1.19 features (quantization, memory tiers, per-tenant IDF, tiered
  multitenancy) are adopted under U-KN-05.
- **Security (U-DATA-08).** API keys, with a read-only key or JWT access control for the Knowledge
  API (read) and the Knowledge worker (write), and TLS. Network segmentation: a `data` network
  that only the services owning each store join; the web edge and the sandbox pool join none of
  them (U-PE-22).
- **High availability (Ph3).** Distributed mode with three nodes and replication factor 2 for
  Kubernetes profiles.

### 4.7 Backup and disaster recovery (U-DATA-06)

| Store | Mechanism | Target |
| --- | --- | --- |
| PostgreSQL | Continuous WAL archiving plus base backups to object storage (pgBackRest or WAL-G; on Kubernetes, CloudNativePG with the Barman Cloud plugin). Encrypted. | PITR; RPO ≤ 5 min |
| Qdrant | Collection snapshots to object storage, recorded with the PostgreSQL publication generation they match | Restore to a consistent generation; re-embedding only as a last resort |
| Object storage | Versioning plus a replica bucket or site | Covers blobs, DocIR and snapshots |
| Valkey `coord` | AOF; loss is tolerated by design (leases expire, sessions re-authenticate) | No backup needed |

Restore drills every quarter rebuild a full environment from backups and measure RPO and RTO
against README §8. The authority ledger is part of the PostgreSQL backup, and `verify` runs after
each restore.

### 4.8 Object storage as the default (U-DATA-09)

- Shared blobs (images, files, artifacts, DocIR, snapshots) go to S3-compatible storage by default
  in multi-instance profiles. `local` remains for single-node development.
- Self-hosted choices: Ceph RGW (strict S3 semantics, large estates), SeaweedFS (many small
  objects, Apache-2.0) or Garage (small clusters). RustFS stays in the local profile; pilot it
  against the platform's S3 operations before any production use, because its S3 coverage is a
  tested subset (WEB). Do not adopt MinIO Community Edition: upstream
  stopped publishing images in October 2025 and archived the server repository in 2026 (WEB).
- Server-side encryption, lifecycle rules per prefix, short-lived presigned URLs, and a one-time
  migration tool from local volumes with checksums.

### 4.9 Pooling and growth (U-DATA-10, U-DATA-11, U-DATA-12)

- **Pooling (U-DATA-11).** PgBouncer in transaction mode, with protocol-level prepared statement
  support, in front of PostgreSQL once services run more than one replica. One pool per role.
- **Migration safety (U-DATA-12).** Lint successor SQL for lock-heavy operations (Squawk or
  equivalent). Require `lock_timeout`, `CONCURRENTLY` index builds and expand/contract steps. Run
  N-1 application compatibility tests per successor (pairs with U-RT-05).
- **Analytics tier (U-DATA-10, optional, Ph3).** Only if dashboards measurably load the primary:
  replicate usage, trace and eval facts through logical replication or the outbox into ClickHouse,
  or into Parquet on object storage queried with DuckDB. PostgreSQL stays the system of record.
- **Lite profile (U-DATA-13, optional experiment).** For single-node installs, test
  PostgreSQL-only retrieval (pgvector 0.8 with VectorChord) against the retrieval golden set. Adopt
  it as an install profile only at parity. The standard profile keeps Qdrant.

## 5. Technology upgrades

| Item | Current | Target | Notes |
| --- | --- | --- | --- |
| PostgreSQL | 16 | 18 (Ph1), 19 (Ph3 after its first minor release) | Checksum decision in rehearsal |
| Kubernetes operator | None (Helm StatefulSet) | CloudNativePG with the Barman Cloud plugin | In-tree Barman support is deprecated (WEB) |
| Pooler | None | PgBouncer, transaction mode | Before multi-replica |
| Coordination | Redis 7, one instance, `allkeys-lru` | Valkey 9: `coord` (`noeviction`) and `cache` | F-40 |
| Vectors | Qdrant 1.18.2, no auth | 1.19.2, API keys or JWT, segmented network | F-39 |
| Blobs | Local volumes | S3-compatible by default for multi-instance | Not MinIO CE |
| Backup | `pg_dump` | PITR, Qdrant snapshots, object versioning, encryption | F-38 |

## 6. Work packages

| ID | Work package | Prio | Phase | Acceptance |
| --- | --- | --- | --- | --- |
| U-DATA-01 | PostgreSQL 16 → 18 through the authority, rehearsed on a copy | P1 | Ph1 | Authority `verify` passes on the upgraded copy; measured downtime; rollback plan tested |
| U-DATA-01b | PostgreSQL 18 → 19 after GA and its first minor | P3 | Ph3 | Same as above; `REPACK CONCURRENTLY` replaces manual bloat work |
| U-DATA-02 | Per-service roles and schema-scoped `search_path` in every profile (F-05) | P1 | Ph1 | No superuser in any service configuration (CI check); negative cross-schema tests |
| U-DATA-03 | Time partitioning and partition-based retention (F-21) | P2 | Ph2 | Online migration rehearsed on a restored snapshot; retention without large `DELETE`s |
| U-DATA-04 | Row-level security for tenant tables (F-21) | P2 | Ph2–Ph3 | Cross-tenant negative suite at the SQL layer; plan regressions measured |
| U-DATA-05 | Valkey 9 with separate `coord` and `cache` instances, ACL users, TLS (F-40) | P1 | Ph1 | Memory-pressure test: no silent loss of leases or sessions; admission fails closed |
| U-DATA-06 | PITR, Qdrant snapshots, object versioning, encrypted backups, quarterly drills (F-38) | P1 | Ph1 | Drill meets RPO ≤ 5 min and RTO ≤ 30 min |
| U-DATA-07 | Qdrant 1.18.2 → 1.19.2 | P2 | Ph1 | Snapshot, upgrade, retrieval golden set unchanged |
| U-DATA-08 | Qdrant API keys or JWT, TLS and network segmentation (F-39) | P1 | Ph1 | Unauthenticated access refused; only Knowledge roles reach Qdrant |
| U-DATA-09 | S3-compatible object storage by default for shared blobs | P2 | Ph2 | Local-to-S3 migration tool with checksums; multi-replica Gateway serves the same blobs |
| U-DATA-10 | Optional analytics tier (ClickHouse or Parquet with DuckDB) | P3 | Ph3 | Only with evidence of primary load; dashboards moved off the primary |
| U-DATA-11 | PgBouncer pooling per role | P2 | Ph2–Ph3 | Connection count bounded under replica scale-out |
| U-DATA-12 | Migration lint, lock timeouts, N-1 compatibility tests per successor | P2 | Ph1–Ph2 | Lint gate in CI; N-1 suite green per successor |
| U-DATA-13 | Lite profile experiment: PostgreSQL-only retrieval | P3 | Ph3 | Adopt only at golden-set parity |

## 7. Verification gates

- **Authority.** `verify` after every upgrade, restore and successor; no fingerprint patching
  (AGENTS.md).
- **Recovery.** Quarterly restore drills with measured RPO and RTO; Qdrant restored to a
  generation that matches PostgreSQL.
- **Isolation.** SQL-layer cross-tenant tests with RLS; role-capability tests (each service can do
  exactly what its inventory grants); unauthenticated Qdrant and Valkey access refused.
- **Coordination.** Fill tests on `coord` show explicit failures, never silent eviction; lease and
  reservation conservation under restarts.
- **Performance.** `pg_stat_statements` top-N regression check across upgrades; partition pruning
  verified in plans for time-range queries.
