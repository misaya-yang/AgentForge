# Native authority packaged provenance

Read-only inspection: 2026-10-15 14:44 UTC. No database execution.

## Delivery result

The shipped source ZIP and native runtime image can load the existing immutable baseline and epoch metadata without the retired Python generator/preimage files. This is the existing source contract, not a new bypass:

- Original `database/authority/commands.py::load_baseline`, lines 123–141 (recoverable reference), invokes Git provenance only if `repo_root/.git` exists.
- Native `platform-migration-authority/src/commands.rs::load_baseline` retains that same condition. It always validates baseline SQL/policy inventories and hashes, the legacy freeze point, and the baseline manifest SHA.
- The current deliverable root has no `.git`. `deploy/platform-native/Dockerfile` copies `database/` to `/app/database` and sets `AI_GATEWAY_DATABASE_DIR`; it does not copy a source Git repository or Python generators into the runtime image.
- `init-fresh` delegates to `commands::migrate`, which calls the loader before connecting for the canonical fresh flow. Applied epoch references bind the exact baseline manifest hash and every applied change hash; all 17 reference JSON files remain present.
- Application `application_guard::require_compatible_schema` reads the database authority ledger. It does not read generator/preimage paths or invoke Python.

The historical generator `scripts/database/freeze_baseline.py`, source Git SHA, generator hash, frozen SQL and policy hashes remain unchanged. These fields describe historical provenance and are not instructions to execute that generator at startup.

## Git checkout prerequisite

The native producer migration is explicitly bound by `database/authority/native-producer-migration-v1.json`, compiled into `producer_provenance.rs`. It binds the exact baseline manifest SHA256, baseline ID, source commit, seven retired producer paths and their historical SHA256 values, and the current Rust replacement source hashes. Only an absent allowlisted producer can use this route. Its original Git blob must still exist at the recorded source commit and hash exactly, and every bound native replacement must match. Present producers retain the original Git-diff rule. SQL, cutover, role/bootstrap, pending-manifest, ancestor and append-only epoch-history checks are unchanged. The descriptor excludes itself and the verifier from replacement hash inputs, avoiding a self-reference cycle.

An unrelated/shallow checkout missing the recorded source commit still refuses. A native replacement change requires an explicit reviewed migration descriptor update. The packaged no-Git route above is unchanged.

## Historical evidence verified

At 2026-10-15 14:52 UTC, the connected GitHub reader retrieved all seven complete producer blobs from `misaya-yang/AI--Platfform` at `386344c6c15e92547cabe32b07fad304afbf4a98`. Each full base64 payload matched the retained reference bytes exactly; their Git object IDs and SHA256 values are recorded in the descriptor. This verifies historical producer equality, rather than inferring it from the retained archive. Read-only local Git inspections found no checkout containing that commit, so the full real-repository ancestry/epoch-history route has not been executed here. Synthetic Git fixtures test the new bounded verifier; no real Git history was manufactured or changed.

## New regression, pending coordinator execution

`cargo test -p ai-platform-migration-authority --test authority_migration_parity_packaged_source`

`packaged_native_loader_keeps_historical_identity_without_python_preimages` constructs a temporary no-Git package containing only database artifacts, excludes Python files, checks the historical generator is absent, calls the actual readiness/baseline/legacy loader, preserves manifest bytes and hashes, and validates all 17 applied epoch fingerprint references. It performs no DB access. This lane formatted the test but did not compile or execute it; root owns serialized verification.

Additional native unit filter: `cargo test -p ai-platform-migration-authority --lib producer_provenance::tests`. Two offline tests cover the exact real allowlist/current replacement hashes plus a separate synthetic Git fixture: accepted retired producer, native drift, unavailable historical commit, historical hash mismatch, and present changed producer still rejected by original Git diff. These tests are written/formatted and pending root execution. No immutable baseline or SQL artifact was changed.

## Coordinator verification update

Root reported authority80 passed: both `producer_provenance::tests` cases, five
ordinary authority parity cases (one DB case explicitly ignored), four manifest
cases and the packaged-source case. Root independently checked all 11 current
replacement-file SHA256 bindings for the seven retired producers. This does not
claim execution of the full real-checkout historical ancestry route or a DB run.
