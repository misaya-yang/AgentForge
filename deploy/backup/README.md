# Encrypted recovery sets (U-DATA-06)

Native entry point: `recovery-backup` in `platform-release`. This does not replace
`make backup`/`make restore` or the source-only `recovery-pack` command. It does not
start Compose, create an account, mutate schema, provision buckets, or delete volumes.
The public template contains no live credentials. PostgreSQL physical backups cover
**the whole cluster**, including roles and the authority ledger, not just one database.

The native bin is auto-discovered from `src/bin/recovery-backup.rs`; its child module
needs these existing internal dependencies in `platform-release` (primary writer):

```toml
ai-platform-service-primitives = { version = "0.1.0", path = "../platform-service-primitives" }
ai-platform-migration-authority = { version = "0.1.0", path = "../platform-migration-authority" }
reqwest = { workspace = true, features = ["multipart"] }
```

Use the repository-pinned toolchain and primary-controlled build/install procedure.
No new third-party package or service is introduced. pgBackRest, PostgreSQL `pg_ctl`,
and isolated Qdrant/S3-compatible instances are operator-provided prerequisites;
this tool never downloads or starts storage services. The bin requires Unix
ownership/permissions. Register it in the shared install-bin catalog when integrated.

## Plan and execution

Copy `recovery.example.json` to an external public configuration, replacing paths,
ports, identity and bucket names. `repo_root` must identify the current checkout.
Source PG path/port/database/principal must match the actual selected cluster.
`plan`/`validate` read only public config and repository path metadata, never secrets
or a live endpoint; they make no changes and do not claim live validation:

```sh
recovery-backup plan /etc/agentforge-recovery/recovery.json
recovery-backup validate /etc/agentforge-recovery/recovery.json
```

Execution additionally requires `--execute --allow-local-target local-recovery`,
where the target ID must exactly match that configuration. PG DSNs must explicitly
bind `127.0.0.1`, the configured ports/database and a direct `current_user=session_user`.
Qdrant and S3 endpoints are restricted to explicit loopback origins, with no URL
credentials/query/path, redirects, proxy inheritance or remote-DNS fallback. This
implementation is a **local recovery/drill** boundary; use operator-controlled local
endpoints for storage. It does not claim remote cross-site deployment acceptance.

Prepare operator-owned external evidence/drill roots and an **empty** pgBackRest
include directory. Private DSNs, pgBackRest config, API keys and S3 keys are external
regular files, owner-only permissions (0600/0400), same effective UID, no symlink or
hardlink and bounded size. The optional storage CA PEM follows the same file policy.
Never put live values in this repository, arguments, shell history or reports.
Runtime secret file contents and raw command stderr are never printed.

Install the pgBackRest template as an external secret-bearing config. It enforces
S3, AES-256-CBC client encryption, verified HTTPS, one repository/cluster and one
worker. Give backup credentials only the configured repository/object scope. This
is an explicit backup administrator workflow; **application/migrator credentials
must not be promoted or used as an administrative fallback**. The configured source
principal needs backup/WAL restore-point/Knowledge-lock and authority verification
permissions; source and restored DSNs refer to that cloned administrator identity.

The PostgreSQL include template enables continuous synchronous `archive-push` and
60-second WAL switching. The source's exact `archive_command` must match the native
command's configured binary/config/include/stanza. Installing this source-side
include (and any required restart) is a separate primary/operator deployment step;
this tool does not alter the existing PG service. Prepare the stanza before enabling
archiving; install its secret config and storage CA first. `stanza-create` is
idempotent; `check` and settings validation fail closed on wrong/missing setup.

Source and replica buckets require Enabled versioning and default AES256/KMS
storage encryption. Install bucket-wide, enabled replication using the public XML
template and a provider-specific replication role. The code verifies the exact
replica destination and empty filter; it does not silently enable a weaker policy.
The configured `object_prefix` must cover the deployment's blobs and DocIR keys.
Encryption keys, both PG and object-store recovery credentials, and the source
checkout/authority artifacts must be kept separately in the operator's recovery kit.

```sh
# Full encrypted PG base backup, plus a new consistent publication recovery point.
recovery-backup backup /etc/agentforge-recovery/recovery.json --execute --allow-local-target local-recovery
# Between full backups: reuse the existing physical base; archive continuous WAL
# and capture a fresh publication/Qdrant/object recovery checkpoint.
recovery-backup checkpoint /etc/agentforge-recovery/recovery.json --execute --allow-local-target local-recovery
```

The returned evidence directory contains the source authority verify, bounded
pgBackRest info, named restore point/LSN, actual archived WAL identity and replica
versions, database system ID, ordered dataset/tenant/collection publication
revisions and their digest, Qdrant snapshots/checksums and both object version IDs.
The encrypted versioned recovery-set manifest is also uploaded and verified on the
replica. A full backup verifies every physical base-backup object's replica; each
checkpoint verifies current pgBackRest indexes and its recovery-point WAL replica.

While establishing a checkpoint, `knowledge.datasets` is locked against changes
and the existing dataset-index publication advisory locks are acquired, refusing
active publishers or negative revisions. Locks cover Qdrant snapshots and generation
binding through the PG restore point. Unrelated databases are not locked. Published
object keys must follow the existing immutable-key contract; mutable external
objects require their writer's own fence before taking a checkpoint. No unverified
claim of a universal transaction across unrelated object writers is made.

Qdrant snapshots/objects stream through private external staging files and encrypted
S3 storage. Use an encrypted local evidence/staging filesystem; staging is not
application-layer encrypted. Single snapshot/object PUTs are bounded to 5 GiB;
larger collections require a separately implemented multipart archive path and are
refused rather than buffered or partially declared successful. File staging is
removed only after successful transfer; failed partial files remain private for
operator inspection. Only the exact new Qdrant source snapshot created as staging by this run is deleted
after its encrypted replica is verified. Existing snapshots, persisted backup
objects and drill volumes are never pruned.

Optional systemd units are **not installed or enabled by this change**. The daily
full backup timer and four-minute checkpoint timer both require a manually prepared
`backup-enabled` sentinel and explicit target ID. Adjust filesystem allowlists and
stanza/config identity before activation. Continuous WAL alone does not establish a
five-minute **cross-store** recovery point; measure actual checkpoint age and
snapshot/replication completion. Large snapshots may miss the target and must be
reported as a failure, not as accepted RPO.

See [quarterly drill](../runbooks/recovery-backup.md) for isolated restore and
measurement. Static source/tests do not establish an executed backup or recovery.
