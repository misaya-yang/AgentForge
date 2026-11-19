# Finite native skip review

Reviewed the exact `hygiene-final61.log` inventory: **57 Rust ignores in 15 files** and **one additional TypeScript marker**. No Cargo, live service, model, browser or device test was run. The active baseline was not edited.

## Proposed artifacts

- `reports/proposals/skip-baseline.native-reviewed.json`: complete candidate baseline with 57 individually named Rust markers; the existing six-marker TypeScript section is structurally unchanged. It incorporates the reviewed quiz smoke rename below.
- `reports/proposals/native-ignore-review.json`: all 57 targets, declared reasons, source SHA-256, prerequisites, classification and evidence limitations, plus separate TypeScript review. Binds the input log and existing baseline bytes.
- `reports/proposals/typescript-live-receipt-skip.proposed.json`: optional seven-marker TypeScript section for a separate explicit root decision. It is not included in the native-only proposal.

## Findings and corrections

The only misleading contract claim found was `quiz_capabilities.rs::postgres_contract_fixture_is_opt_in`: its body merely connected to PostgreSQL and closed the pool. At the root's direction it is now named `postgres_connection_smoke_is_opt_in`, with an explicit reason and comment stating that it checks connection availability only, not migration096 or quiz persistence. It remains ignored and is classified as **one connection smoke**, not accepted quiz contract coverage. No test was removed or made automatically live.

The other 56 ignores correspond to real infrastructure/model prerequisites and substantive direct or helper assertions. Their existence/compilation does not prove execution, full source scenario coverage, production readiness or live acceptance. Required integration gates must still use explicit `--include-ignored` (or individually selected ignored tests) and reject skipped/zero-test results. A baseline entry documents why ordinary offline runs omit a test; it does not waive that test's eventual acceptance obligation.

## Source prerequisite review

| File group | Count | Required environment / effect |
|---|---:|---|
| Runtime `postgres_store_contract_tests` | 1 | Explicit local PostgreSQL, runtime migration path, unique schema, native in-process runtime. Actual thread/store round trip. |
| Capability worker quiz connection smoke | 1 | Explicit disposable `DATABASE_URL`; connection and close only. |
| Studio agents API | 4 | Isolated loopback PostgreSQL from repository `.env`; unique schema and real repository CRUD/revision constraints. |
| Studio boot | 1 | `GATEWAY_BOOT_DISPOSABLE=1`, explicit config, provisioned isolated PostgreSQL/Redis and native subprocess. |
| Studio channel | 2 | Isolated PostgreSQL plus explicitly selected loopback Redis; signed embed/cookie/origin routes. |
| Studio database | 6 | Isolated PostgreSQL, migration071, unique schema; actual DDL/FK/immutability/owner/atomicity constraints. |
| Studio observability | 4 | Isolated PostgreSQL; shared channel limiter additionally needs isolated Redis. Helpers assert audit/usage/legal-hold/limiter behavior. |
| Studio publication | 15 | Unique disposable database created/dropped using configured PostgreSQL admin rights; all stateful cases beyond the first DDL case additionally need isolated Redis. Real release/auth/storage assertions. The selected-live-run test inserts controlled run evidence; it does not run a provider. |
| Studio RBAC | 3 | Isolated PostgreSQL and unique schema; role/owner/tenant constraints. |
| Studio runtime | 2 | Isolated PostgreSQL and Redis; actual token/audience/tenant behavior. |
| Confluence SQL | 1 | `KNOWLEDGE_TEST_DATABASE_URL`, canonical `knowledge.documents` schema; mutations are in a session-local temporary table. |
| Native BGE inference | 1 | Pre-provisioned official model artifacts via `KNOWLEDGE_TEST_BGE_MODEL_DIRECTORY`; actual local CPU inference, no implicit download. This is an external-artifact prerequisite, not a network/live-service claim. |
| Qdrant integration | 2 | Real disposable Qdrant, unique collections, hybrid/scope/deletion assertions. The reviewed safety fix now requires a nonempty explicit `QDRANT_URL` before any connection; no localhost fallback remains. Deliberate `--ignored` invocation and a disposable service remain prerequisites. |
| KB migration authority | 5 | Explicit database-create opt-in and admin DSN; unique databases. Subprocess cases need native authority; crash case signals its own child; restore case requires `pg_dump`/`pg_restore`. |
| Runtime thread-store migration | 9 | Explicit isolated PostgreSQL settings or selected env file, migrations089–095, unique schemas; actual import/append/scope/model-lease SQL contracts. |

Studio fixtures reading `.env` are not blanket permission to use a developer's or production database. Their execution must explicitly select a disposable environment; some publication/auth paths need a fully disposable database rather than assuming a random search-path schema isolates qualified tables. No `.env` or credential contents were read in this review.

## Additional TypeScript marker

`web/e2e/agent-eval-receipts-live.spec.ts:8` uses the conditional `test.skip(!fixturePath, ...)`, where the path comes from `R3_ACCEPTANCE_FIXTURE_FILE`. It exercises four existing evaluation-run receipts plus one authorized analytics-to-trace navigation (five tests), with real page assertions and screenshot output. It does not dispatch candidate runs or provision fixture records. An existing receipt JSON, `E2E_BASE_URL` and the configured authenticated Playwright state are genuine prerequisites. The skip is justified as a conditional live receipt-reading gate, subject to root approval of the separate proposal. It is not unconditional disabled coverage, but it remains unexecuted here. The native-only proposal preserves all six historical TypeScript markers unchanged.

## Integration caution

The input log names the old quiz target/reason; the proposal deliberately names the corrected current source. Re-run static hygiene after root integration rather than treating the historical log as a passing result. Existing no-assertion warnings were not blindly suppressed: several reviewed functions use assertion helpers; the quiz case was explicitly downgraded to smoke. No new baseline entry should be described as evidence that an ignored test passed.

## Delivery checkpoint review, 2026-10-15 15:56 UTC

The cached native hygiene gate found no hard/self-proving-test failures and exactly 13 new ignore markers. These are reviewed external prerequisites, not passes. The native ledger now contains 78 Rust markers; historical baseline and existing TypeScript markers are unchanged. No release gate or test body was changed.

- `gateway_migration_parity_auth_database` (7): missing-user no-JIT login; cache MISS/HIT bytes; verified-credential tenant/profile projection; inactive/moved-account rejection; image claim/dispatch fencing; atomic accounting commit-once/replay; rollback/quarantine. Every case invokes `isolated`, requiring `AUTH_PARITY_DISPOSABLE=1`, loopback `AUTH_PARITY_ADMIN_DATABASE_URL` and `AUTH_PARITY_REDIS_URL`, and creates a uniquely named owned database. The two accounting cases explicitly retain unrun atomic receipt/rollback acceptance. Run only in an approved disposable fixture with `cargo test -p ai-platform-gateway --test gateway_migration_parity_auth_database -- --ignored`.
- `gateway_migration_parity_task_authorization` (4): concealed denied routes; same-tenant owner/admin access; scoped list/missing-resource side effects; exact-snapshot cancellation. Every case calls the real Redis-backed auth setup requiring `AUTH_PARITY_DISPOSABLE=1` and explicit loopback `AUTH_PARITY_REDIS_URL`; PostgreSQL is lazy/unused in this fixture. Run only with approved disposable Redis using `cargo test -p ai-platform-gateway --test gateway_migration_parity_task_authorization -- --ignored`.
- `runtime_migration_parity_events` (1): real publish/duplicate/poison/retry/dead-letter lifecycle requires explicit `AI_PLATFORM_RUNTIME_EVENT_TEST_REDIS_URL`, loopback, nonzero database, and empty DB guard. Run `cargo test -p ai-platform-gateway --test runtime_migration_parity_events -- --ignored` only against a dedicated disposable Redis DB.
- `runtime_migration_parity_session_tombstone` (1): actual PostgreSQL delete/tombstone/audit-chain constraint requires `AI_PLATFORM_RUNTIME_PERSISTENCE_TEST_DATABASE_URL`, loopback database named `runtime_parity_*`, no application tables or preexisting assistant schema; cleanup targets only its created schema. Run `cargo test -p ai-platform-gateway --test runtime_migration_parity_session_tombstone -- --ignored` only against the approved empty disposable DB.

All 13 remain live-unverified under the owner's no-live-execution checkpoint. Ordinary offline omission must never be used as successful release acceptance. Existing required acceptance gates remain required; the explicit target invocations above identify the additional assertions still awaiting infrastructure execution. Helper-only assertion warnings were not suppressed.

Post-checkpoint126 review: `accounting_failure_conflict_monotonically_quarantines_uncertain_commit` uses the already-reviewed `isolated` disposable loopback PostgreSQL/Redis fixture and transaction-local accounting tables. It verifies safe→uncertain→safe receipts cannot restore replay permission. The owner’s live-acceptance pause remains active; this is an unexecuted prerequisite, not a passing test. Explicit later command: `cargo test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --test gateway_migration_parity_auth_database accounting_failure_conflict_monotonically_quarantines_uncertain_commit -- --ignored --exact --test-threads=1`, after supplying its disposable fixture URLs.
