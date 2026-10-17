# U-DATA-01 PG16 → PG18 rehearsal

Implements [plan 07 §4.1](../../docs/plans/platform-upgrade-2026-10/07-data-platform.md).
Configuration and complete commands are in [the owning deployment directory](../backup/pg-upgrade/README.md).

Before any real data operation, the primary/operator follows the existing
[runtime/secret rules](../../docs/harness/runtime-and-secrets.md), verifies Compose
ownership where relevant and allocates one controlled resource slot. Prepare an
independent cold copy from the selected development volume, its separate preserved
tablespaces, short external working root and both operator-installed major-version
binary/extension sets. Preserve all owned source volumes. This tool does not create
or start infrastructure and does not read private env or credential files.

Primary integration/checks (through the canonical toolchain/external target rules):

```sh
cargo test -p ai-platform-release --bin pg-major-rehearsal
pg-major-rehearsal plan /EXTERNAL/pg-rehearsal.json
```

The bin uses the existing `ai-platform-migration-authority` internal dependency
requested for recovery-backup; no further manifest/dependency change is needed.
Add `pg-major-rehearsal` to the shared native install-bin catalog. Static tests cover
unsafe option paths, foreign copy roots, shared ports, source hardlinks, byte drift,
nonallowlisted symlinks and dirty/mismatched checksum control states. They do not
simulate a passing PG18 catalog or replace real native verification.

Run explicit `inspect-copy` and then `run` with the exact configured copy ID using
the commands in the README. Inspect neither starts nor fixes the source: clean
shutdown and PG_VERSION16 are required. Existing sealed-copy receipts cannot be
overwritten. A changed preserved copy requires a new deliberate preparation and new
receipt path, never editing a failed hash to declare acceptance.

Every rehearsal uses a fresh random workspace. `started.json` is written before
native execution. Command logs, server logs, frozen-source binding, actual source16/
upgraded18/rollback16 authority results and actual-vs-frozen fingerprint maps stay
in this private external workspace. The final receipt is written both there and to
the configured evidence root. Preflight/copy failures retain a failed receipt when
the workspace has been created; pre-admission refusal has no database effects.

A successful receipt requires all of:

- PG18 actual native authority verify and equality with every frozen fingerprint.
- An actual independent PG16 rollback clone, successful native verify, correct
  checksum setting/page checks when enabled, and copy shutdown.
- Unchanged preserved-source tree SHA256 and unchanged active authority baseline/
  epoch source hashes, with no link into preserved-source data.
- All owned rehearsal copy servers stopped. Cleanup errors cause failure and are
  reported; investigate only those private owned paths, never a main volume.

`downtime_ms` uses a monotonic clock from the actual link upgrade through PG18
startup and successful authority verification. Initial byte copying and the
`pg_upgrade --check` preflight are deliberately excluded; the receipt names that
scope. If upgrade/verification fails, the observed failed-attempt duration is still
recorded. Rollback copy/start/verify has its own real elapsed time. This measures a
copy rehearsal, not a production service outage or cutover SLA.

A fingerprint/catalog difference is expected to be a possible **real blocker**
across major versions. Keep `authority-upgraded18.json`, its actual/frozen maps and
private pg_upgrade logs. The independent rollback result remains separately visible
when upgrade fails. Do not rewrite immutable SQL, manifests, reference snapshots,
source checksums or fingerprints to make the report pass. Missing target extension
libraries, unsupported locale, dirty source, checksum mismatch, process timeout or
rollback failure likewise remain failures.

After a native rehearsal passes, production rollout still requires the primary's
coordinated backup/PITR evidence, provider/tenant/application regression, capacity
and planner/IO measurements, full topology integration, approved cutover/rollback
window and real service availability measurements. This command does not switch an
application DSN, stop a main database, run migrations, delete copies or mutate an
owned production volume. Preserve failed/successful private workspaces until the
operator explicitly decides their retention.
