# 07 Data platform (database authority, PostgreSQL, Valkey/Redis, Qdrant, object storage)

按完整模块交付；进度只在[现有账本](../../../deploy/runbooks/native-enterprise-upgrade/loop-state.json)。
原始工作包用于需求与验收核对，执行顺序使用[主计划的三个批次](README.md#6-roadmap)。

## 1. 本模块要交付什么

提供结构完整、权限正确、可持续读写与恢复的数据底座，使所有服务能在同一开发环境使用真实数据完成主流程。

| 交付部分 | 原需求 ID | 连通的结果 |
| --- | --- | --- |
| 结构、身份与连接 | `U-DATA-02`, `U-DATA-04`, `U-DATA-11`, `U-DATA-12` | 完整表/字段/索引、服务角色、租户上下文、连接池和 authority 调用贯通。 |
| 在线存储能力 | `U-DATA-03`, `U-DATA-05`, `U-DATA-08`, `U-DATA-09` | 分区/保留、coord/cache、Qdrant 安全与共享对象存储由实际业务路径使用。 |
| 升级与恢复 | `U-DATA-01`, `U-DATA-06`, `U-DATA-07` | 升级、备份和恢复工具实现完成；历史库演练及 RPO/RTO 在集成验收阶段执行。 |
| 条件项目 | `U-DATA-01b`, `U-DATA-10`, `U-DATA-13` | PG19、分析层和 Lite 检索保留各自版本/负载/质量触发条件，不默认拉入当前开发。 |

以上部分在同一模块中连续完成，不分别创建小任务、审批、审核或交接记录。

## 2. 接线与真实依赖

新开发库直接由 native authority 初始化当前完整结构；不要把旧库迁移编号和多版本演练当作每次启动前置任务。历史升级/兼容性要求仍需验收，冻结输入与原数据不能改写来绕过失败。

## 3. 一次集中验证与收尾边界

一个真实数据库批次验证结构、索引、服务角色、跨租户拒绝与事务复用。随后复用同一集成候选完成存储/升级/恢复场景；只在相关 schema、权限或配置变化后重跑受影响场景。

不逐表另开任务、报告或环境；不为验证工具创建重复基础设施。已有数据保护、权限和恢复要求保留。

<details>
<summary>设计约束与技术选择（实现涉及该边界时按需查阅）</summary>

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

</details>

## 6. Work packages

<details>
<summary>原始需求和验收对照；Prio / Phase 是原计划标签，不是逐项执行流程</summary>

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

</details>

## 7. Verification gates

以下场景在上述模块验证和最终验收批次中覆盖。同一场景由一个执行者运行一次，关联
多个需求即可；未变化的适用结果直接复用。关键边界和原门槛不能由重复的局部测试替代。

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
