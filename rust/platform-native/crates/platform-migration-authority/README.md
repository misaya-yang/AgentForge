# Native database authority

`database-authority` ports the existing `database.authority` command surface.
It consumes the same frozen `database/baselines`, immutable legacy/per-service
manifests, canonical epoch manifest and verification files. It does not invent
another migration history or advisory lock.

The database artifact directory defaults to the repository's `database` directory.
Packaged execution can set `AI_GATEWAY_DATABASE_DIR` to its immutable artifact
mount. `AI_GATEWAY_ROLE_PREFIX` follows the existing bounded lowercase prefix
contract. Every command uses its explicit `AI_GATEWAY_DATABASE_<PRINCIPAL>_DSN`;
there is no shared `DATABASE_URL`, inferred password or retry-as-admin fallback.

Commands: `migrate`, `init-fresh`, `cutover`, `status`, `source-kind`, `verify`,
`startup-check`, `fingerprint`, `provision-roles`, `bootstrap-ledger-acl`,
`prepare-cutover-ownership`, and `verify-role-connections`.

- Baseline installation and transactional epochs commit DDL plus the canonical
  success marker atomically. Fresh installation verifies every fingerprint before
  writing its marker.
- Authority writers hold the existing session advisory lock (1095781959, 1).
- Role bootstrap is explicit admin work; ordinary migration verifies its result.
- The ledger ACLv1 digest, receipt, exact rights and ACL delta remain unchanged.
- `status`, `verify`, `source-kind` and `startup-check` connect read-only.
- `startup-check` retains the Python command's reviewed principal windows:
  Gateway/Runtime/Capability Worker 2–10; Knowledge API/Worker 5–17. Actual native
  application startup guards remain independently owned contracts.
- Nontransactional execution requires explicitly registered recovery callbacks.
  The CLI registers none, as the Python CLI does. The current canonical epoch
  has only transactional changes. An executable digest change refuses recovery
  of older open attempts instead of assuming state-machine compatibility.
- `--reconciliation-evidence-out` is the only way CLI commands write a receipt
  file. Database credentials and driver error text are not emitted in receipts.

Implementation is not deployment acceptance. No production migration or cutover
is authorized by compilation alone. Isolated real-PostgreSQL fresh, legacy,
epoch, wrong-role, concurrent-runner, drift, rollback and interruption tests are
required before changing a deployed authority. Library cancellation during a
nontransactional session-scoped operation requires explicit review/testing;
callers must discard the dedicated connection rather than reuse a cancelled one.

See `reports/rust-migration-foundation.md` for the worker's actual test status.

## Source and acceptance tools

The same crate provides `check-successor-proposals`, `measure-schema-successors`,
`render-baseline-contract`, `database-policy`, `generate-database-grants`,
`freeze-baseline`, and `probe-successor-roles`. Each supports `--help`. Policy/grants and render commands
check for drift by default unless an output/write option is explicit.
`freeze-baseline` requires a clean pending source commit and two distinct
PostgreSQL 16 scratch databases, reproduces the baseline before publication,
and writes the activation manifest last. It never creates or resets databases.
`probe-successor-roles` is offline by default. Its two separately named opt-in
flags do not authorize an operator to run it without permission for the exact
isolated database and synthetic fixtures; sequence increments may remain even
though probe rows and temporary objects are rolled back.

For a source-bound external preview, run:

```sh
cargo run --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin check-successor-proposals -- \
  --root "$SOURCE_CHECKOUT" --preimage-root "$PRESERVED_CHECKOUT" \
  --preview-output "$NEW_EXTERNAL_DIRECTORY"
cargo run --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin measure-schema-successors -- \
  --source-root "$SOURCE_CHECKOUT" --preview-root "$NEW_EXTERNAL_DIRECTORY" \
  --preimage-root "$PRESERVED_CHECKOUT" \
  --expected-database "$ISOLATED_DATABASE" \
  --run-approved-isolated-measurements
cargo run --manifest-path rust/platform-native/Cargo.toml \
  -p ai-platform-migration-authority --bin probe-successor-roles -- \
  --root "$NEW_EXTERNAL_DIRECTORY" --source-root "$SOURCE_CHECKOUT" \
  --preimage-root "$PRESERVED_CHECKOUT"
```

The output directory must not exist and must be outside the source checkout.
The probe's `--root` supplies the candidate database artifacts; `--source-root`
binds production requeue SQL and frozen preimages to the original checkout. Its
default remains the candidate root for single-root compatibility. The optional
`--preimage-root` must be the preserved checkout named by the source-import
receipt; it is read only to verify the one retired frozen preimage and is never
copied into the preview. The measurement command is the only command above that
connects to PostgreSQL; it requires explicit approval and a named isolated
database, and writes only into that database and the external preview. Before
measurement, preview verification is offline and requires missing 018–020
references; after measurement, it requires the explicit no-op receipt and exactly
the three source-bound verification references. The final role probe stays
offline unless its separately named probe flag and exact fixture arguments are
supplied.

After an explicitly reviewed activation, `check-successor-proposals` verifies
the active epoch-20 chain from `SUCCESSOR_ACTIVATION.json`; requesting a new
preview then refuses. The historical inactive proposal remains unchanged.
Activation verification binds the retained epoch-17 manifest and package lock,
the measurement receipt, and the three measured successor references under the
release-pinned migration directory. A single-root role probe validates that
same active receipt before database access.

Native policy scanning maps Knowledge integration modules to Knowledge roles,
not the gateway crate name. Shared API/worker modules produce shared static
evidence, which still needs statement-level review before a new live freeze.
Immutable existing baseline artifacts are not regenerated by builds or tests.
