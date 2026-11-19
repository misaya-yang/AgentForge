# Native legacy numeric reconciliation

`platform-migration-authority/src/numeric.rs` and `numeric_sql.rs` replace the
runtime behavior of `database/authority/numeric_evidence.py` and
`numeric_reconciliation.py` with Rust/sqlx. No Python subprocess, dynamic import,
ledger update, migration execution or administrator-price mutation is performed.

The four historical evidence statements are preserved byte-for-byte, including
schema/OID identity, duplicate-relation refusal, exact constraints/indexes,
backfill checks, trigger ownership and hierarchy referential evidence. A Rust
unit test compares all four strings against immutable golden SHA-256 values
captured from the source specification. Tests require no Python files/interpreter.

The native policy preserves unique-version rows, approved rename aliases,
full/16-digit checksum identities, dirty/duplicate/unknown refusal, the sole
first-name/last-checksum historical overwrite case, and revision-specific
016/030/031 evidence decisions. Missing 031 administrator price history and any
rollback/mixed/unproven state remain blocked, never automatically replayed.

Public function:
`reconcile_numeric_legacy_history(&mut PgConnection, &LegacyManifest) -> Result<Value>`.
Successful result is the existing numeric receipt shape. A blocked receipt is
returned through `AuthorityError::Blocked` as its full JSON string, as requested
by the coordinating legacy caller. Known PostgreSQL SQLSTATE failures are mapped
to the prior asyncpg exception names; other SQLSTATEs become `PostgresError`, and
transport/decode failures become `InterfaceError`, without leaking driver details.

Tests cover overwrite direction, unknown/bare/checksum rejection, Python title
names, absent price migration, rename aliases, dirty/duplicate/unknown versions,
rollback/mixed evidence, complete effective evidence, and exact SQL preservation.
Compilation and tests are owned by the coordinator's serialized build lane and
are pending. Live legacy database reconciliation has not been exercised here.
