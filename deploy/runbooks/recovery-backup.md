# Quarterly isolated recovery drill

Owns U-DATA-06 in [plan 07 §4.7](../../docs/plans/platform-upgrade-2026-10/07-data-platform.md).
Configuration and native integration are documented in [deploy/backup](../backup/README.md).
Keep the existing program's source-bound acceptance records; do not treat this new
runbook or a plan result as measured disaster-recovery evidence.

1. The primary operator checks the existing Compose ownership labels and resource
   budget under the runtime/secrets rules. Do not create a second main stack or
   operate unrelated containers. Prepare an isolated empty Qdrant endpoint, a
   dedicated empty `af-drill-*` object bucket, unused PG loopback port and external
   writable drill root. No service is started by this preparation command.
2. Install/validate the two internal dependencies and multipart feature, then run
   `cargo test -p ai-platform-release --bin recovery-backup` through the shared,
   serialized, external-target build procedure. Tests are synthetic policy checks.
   A compiler/test pass alone says nothing about a real backup.
3. Validate the public configuration with `recovery-backup validate PUBLIC.json`.
   Verify independently that a full encrypted base backup exists, continuous WAL
   archiving works, both buckets are versioned/encrypted, and the configured replica
   is independent. Confirm current source authority verification is available.
4. Run a fresh explicit `checkpoint` (or initial `backup`) from the README and select
   its external `recovery-set.json` path from the command receipt. Do not fabricate a
   freshness timestamp or edit the manifest. Its checkpoint must be recent enough
   for the five-minute target; restore retains real age even for older sets.
5. Run the exact isolated drill, preserving the emitted evidence path:

   ```sh
   recovery-backup drill /etc/agentforge-recovery/recovery.json --execute --allow-local-target local-recovery --manifest /EXTERNAL/RECEIPT/recovery-set.json
   ```

The command creates a **new** 0700 destination for every run. It refuses a busy PG
port, the source PG port, source Qdrant origin, any nonempty Qdrant instance or object
bucket, foreign-scope manifest, unsafe selectors or changed generation digest.
There is no `--delta`, reset/delete/force or in-place restore mode. pgBackRest restores
from the replica repository with all tablespaces remapped under the new directory;
source symbolic-link restoration is disabled. The server's persistent archive-get
command is bound to the replica as well. PostgreSQL binds only loopback; source
archiving/preload/recovery-end and logical-replication hooks are disabled in the
drill process. A new isolated HBA permits only the expected cloned administrator
and database over loopback with SCRAM; source LDAP/PAM/HBA hooks are not reused.

The drill verifies source system lineage, reaching/promoting the named PITR LSN,
exact restored publication-generation digest, and the **real existing authority
verify** before data-store acceptance. It downloads pinned replica object versions
with ETag checks into the dedicated bucket (conditional no-overwrite PUT), preserving
MIME/user metadata, and uploads SHA256-verified Qdrant snapshot files to the isolated
instance. Only the PG process started for this fresh destination is stopped on normal
completion or errors. Data/evidence are preserved; Qdrant/bucket infrastructure
lifecycle remains operator-owned. Failure may leave partial isolated objects and
collections; choose new isolated destinations next time, never delete existing data
to make a rerun pass.

`drill-measurements.json` records:

- Observed disaster time when the drill starts, the real checkpoint time and their
  difference as RPO. Old backups do not become fresh because a drill is run later.
- Monotonic elapsed restore/verification time as RTO, completion timestamp, paths,
  canonical authority verification and generation match, object/snapshot counts.
- Targets **RPO ≤300 seconds, RTO ≤1800 seconds**; a missed target produces a failed
  command with the measured report retained. A workflow timeout retains failure
  evidence and partial isolated data, without claiming a completed restore.

After the native data restore, the primary operator must still run application smoke
queries/retrieval/citations and blob/DocIR readers against only these restored
endpoints; verify tenant-denial cases and a real documented publication generation.
Do not start application workers pointed at production providers or claim that SQL
metadata, a checksum or Qdrant's upload success proves end-user retrieval. A whole
quarterly drill is accepted only after those smoke tests and measured RPO/RTO pass.

Retain the source/config artifact identities, completed/failed receipts and secret-free
summary in the existing program evidence. Keep raw backup data, private manifests,
credentials, staged payloads and PG logs outside source control. Schedule/repeat
quarterly with fresh isolated targets; this repository change does not schedule or
execute a drill automatically.
