# PG16 → PG18 isolated-copy rehearsal (U-DATA-01)

Native bin: `pg-major-rehearsal`, auto-discovered in `platform-release/src/bin`.
It reuses the existing native authority verifier/fingerprint implementation and the
internal authority dependency already requested for `recovery-backup`; no new
third-party dependencies or service are needed. Primary integrates its shared
install-bin catalog and builds/tests. No Cargo/DB/container operation is run by this
source change. `recovery-backup`, `make backup` and frozen SQL remain untouched.

Prepare a **cleanly shut down, independent development-volume copy** outside the
checkout. Never point `preserved_source` at an owned production/development source
volume. The operator creates the initial copy under a dedicated allowed copy root;
this command never stops, starts, modifies, links from, or removes the preserved
source. It only reads source files and runs read-only `pg_controldata` on it. If the
source is live/dirty, has a `postmaster.pid`, has the wrong PG major, or changes after
sealing, execution fails. A copied stale PID/unclean control state is not silently
removed or fixed; prepare a valid cold copy first.

Copy the public JSON template outside the checkout and adjust current source,
repository, toolchain and role/database paths. `allowed_copy_root` must be short,
absolute, operator-owned and preferably mode 0700 (Unix socket path limits apply).
Prepare its evidence directory. Both PG16 and PG18 binaries and **matching PG18
extension libraries** must already be installed by the operator; no download,
extension install, catalog rewrite or automatic fallback happens. Run as the
non-root PostgreSQL copy owner. Bin/work paths accept only safe ASCII path
characters. Three non-default, distinct ports identify private Unix sockets; the
rehearsal servers have TCP listening disabled.

```sh
# Default safe mode: read public configuration/path metadata only, no processes/DB.
pg-major-rehearsal plan /EXTERNAL/pg-rehearsal.json
# Explicit read-only inspection of the stopped preserved copy; writes a new 0600
# receipt OUTSIDE that copy, never overwrites an earlier receipt.
pg-major-rehearsal inspect-copy /EXTERNAL/pg-rehearsal.json --execute --allow-copy pg16_copy_rehearsal
# Explicit rehearsal, with a fresh random workspace for every attempt.
pg-major-rehearsal run /EXTERNAL/pg-rehearsal.json --execute --allow-copy pg16_copy_rehearsal
```

The inspection receipt binds the actual full tree SHA256/file count/size, source
path, selected target ID, PG16 system identifier, checksum setting and clean control
state. Regular files stream through bounded buffers. Device files and arbitrary
symlinks are refused. An external `pg_wal` link must be materialized in the preserved
copy **before** inspection. Tablespace links are supported only when each OID maps
to an explicit, separately preserved tablespace copy under the allowed root in
`tablespaces`. No live/original tablespace path is inferred. Each working/rollback
tablespace is independently copied and its link rewritten inside the fresh workspace.

Execution makes new PG16 working data/tablespace directories with independent
inodes, verifies logical tree SHA256/permissions and data-byte counts, and runs
native PG16 authority verify before attempting upgrade. Source/template1 encoding,
libc/ICU provider and locale are read from this real copy to initialize PG18. It
preserves checksum state explicitly: `--data-checksums` for enabled PG16, otherwise
PG18 `--no-data-checksums`. Enabled page checksums are checked on working copies
before upgrade, after upgrade and during rollback; disabled checksums are reported
as disabled, never silently enabled or described as verified page checksums.

`pg_upgrade --check --link` runs first; only then the actual `pg_upgrade --link`
runs with one job, isolated data directories, Unix sockets, controlled startup
options and copy-only tablespace paths. Device/inode checks prove the new cluster
shares no data-file inode with the preserved source and that real link-mode data
links exist between working copies. There is no invocation of generated cleanup
scripts and no `rm`/volume pruning. Data directories and private logs remain.

The copied servers use a new peer-auth mapping for the current OS owner to the
explicit cloned administrator role, only in a private Unix socket directory. TCP
and inherited archive/recovery hooks, preload modules and logical replication
workers are disabled for the rehearsal. No password, DSN or private env file is
required or read. These safety overrides apply only to copies and are not a
production PostgreSQL configuration proposal.

On PG18 the tool runs the **actual native authority `verify`**, separately computes
actual fingerprints under the canonical owner role, saves them beside the exact
frozen expected fingerprints, and demands equality. A PG18 catalog/ACL/default/
collation/extension difference is an actual rehearsal failure. It never recalculates
a new accepted baseline, rewrites a checksum/ledger, grants rights or ignores a
fingerprint domain to pass. Relevant baseline/epoch source trees are bound before
and after the attempt; concurrent frozen-source drift also prevents acceptance.

Rollback is executed **even if PG18 authority verification fails**. Because PG18
startup can modify linked working PG16 files, rollback never starts that old linked
working directory. It independently clones the untouched preserved PG16 source
again, checks its byte digest/inode isolation/checksum state, starts the PG16 copy,
runs real authority/fingerprint verification and stops it. Thus a rollback receipt
means a tested old-major database, not simply that a backup file exists.

See [rehearsal runbook](../../runbooks/pg-major-rehearsal.md) for evidence and scope.
Source implementation and synthetic fault tests are not an executed upgrade or a
production cutover authorization.
