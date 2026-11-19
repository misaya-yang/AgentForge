# R2-A same-execution cross-store test design

`tests/database/test_special_publication_cross_store.py` is an opt-in integration journey for one synthetic scanned document. The candidate point vectors and image bytes are deterministic fixtures. The test exercises production `DatabaseStorage`, `VectorStore`, `ImageStorageService` with its local filesystem backend, and `SpecialPublicationCoordinator` against live PostgreSQL and Qdrant in the **same** execution. It does not exercise an OCR, image embedding, or browser provider.

## Isolation and run contract

- Each invocation creates a random PostgreSQL schema, dataset/document identity, two Qdrant collections, and a local object-store prefix beneath pytest's temporary directory. Teardown removes only these resources.
- The test is skipped unless `KB_SPECIAL_CROSS_STORE_TEST=1`. An opted-in run requires the local PostgreSQL variables from `ENV_FILE` or the repository `.env`, plus `QDRANT_URL` if Qdrant is not on `http://127.0.0.1:6333`. It fails on unavailable live stores rather than reporting a pass.
- Before the opted-in run, the operator should apply `docs/harness/runtime-and-secrets.md` ownership and health preflight. Run: `KB_SPECIAL_CROSS_STORE_TEST=1 uv run --all-packages --extra test pytest -q --no-cov tests/database/test_special_publication_cross_store.py`.
- No credential values are logged or embedded in the test. The local object signing string is test-only and protects no external resource.

## Assertions at each cut

| Cut | Durable proof |
| --- | --- |
| Two normal generations | One document advances versions 1→2; serving PG rows and Qdrant IDs match the new generation in both collections; old Qdrant IDs are removed; both historical image objects retain their SHA-256 bytes and version manifests. |
| Preparing, after page upload | The predeclared object key is present with a `preparing` manifest and no negative fence. `abort_preparing` removes that candidate object, marks its execution `error`, and leaves previous body, segment rows, Qdrant IDs, and image bytes intact. |
| First Qdrant upsert, before PG commit | Fault injection stops in-process compensation after a real upsert. The sole running owner is `prepared` under a negative revision; old PG content/rows and old Qdrant/object bytes remain. Fresh coordinator recovery deletes the candidate point and object, closes the ledger `error`, and restores a positive revision. |
| PG commit, before old-point cleanup | Fault injection stops at the old-ID deletion call after the actual PG transaction. The sole running owner is `committed` under a negative revision; PG body, rows, version and manifest are new while old and candidate Qdrant IDs coexist. Fresh coordinator recovery deletes only the frozen old IDs, completes the original execution, preserves all historical object hashes, and restores a positive revision. |
| During old-point cleanup | A second injection runs the real first-collection deletion and then stops. Recovery tolerates the already-missing old ID, clears the other collection, and retains the committed candidate and earlier image bytes. |

The fault hooks delegate normal store calls to the real Qdrant client; they only raise at the named cut. The pre-commit hook suppresses the coordinator's in-process compensator to model an abrupt process loss. The test calls recovery through a newly constructed coordinator but does not kill a worker process or verify runtime restart orchestration.

## Evidence status

- `uv run --all-packages --extra dev ruff check tests/database/test_special_publication_cross_store.py`: passed.
- `uv run --all-packages --extra test python -m py_compile tests/database/test_special_publication_cross_store.py`: passed.
- `uv run --all-packages --extra test pytest --collect-only -q --no-cov tests/database/test_special_publication_cross_store.py`: 1 test collected.
- With `KB_SPECIAL_CROSS_STORE_TEST` unset, scoped pytest reported **1 skipped** by design.

**Live PostgreSQL, Qdrant, and object-store execution is pending the primary agent's runtime preflight and run.** Collection and default skip are not live cross-store evidence.
