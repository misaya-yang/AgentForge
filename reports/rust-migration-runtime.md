# Runtime and framework worker migration

## Scope and implementation

Existing Rust Agent Runtime and capability worker remain intact. Added
`rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine` and its workspace
membership. The separate SDK service lane owns HTTP, session lifecycle and child
supervision and invokes this executable.

Implemented native Rust OpenAI Responses and Anthropic Messages streaming loops;
platform private launch/event protocol; pure tools; selected-dataset knowledge
bridge; governed capability execution and approval polling; provider call-ID
binding; unknown-side-effect fail-closed behavior; usage observations; atomic
native session history. There is no Python subprocess facade.

Read source assembly lock and repository instructions before changes. Overlay
manifest, source lock, deployment wiring and Cargo lock regeneration are left to
the coordinating source-assembly lane.

## Actual coverage and boundaries

The two legacy framework identifiers are accepted. This is native protocol
execution, not either vendor's official SDK. SDK-private SQLite/Claude resume
state is explicitly fenced rather than silently discarded. A verified historical
session migration is not implemented. Anthropic direct API calls use an explicit
16,384 output-token ceiling. Full Claude Agent SDK behavioral equivalence is not
claimed. Arithmetic operators, decimal/scientific and hexadecimal/octal/binary literals
and digit separators are implemented with the original 1e15 bound. HTTP/session/retention parity belongs to the SDK service
lane and is not attested here.

See the new crate's `COMPATIBILITY.md` for the exact child contract and boundaries.

## Verification status

Added Rust tests for ASCII-escaped scope encoding, controls in identities, bounded
arithmetic, incomplete usage, cache counters, state roundtrip/framework fence,
legacy opaque-state rejection, OpenAI loopback SSE completion/incomplete streams,
and Anthropic tool-argument reconstruction.

No Cargo build/test was run in this lane, per serialized-build instruction.
`rustfmt --edition 2024 .../src/*.rs` was attempted and could not run because
`rustfmt` was absent from PATH. These tests are not marked passed. No provider
credentials, paid provider calls, deployments or commits were used.

Requested coordinator gate after assembly: `cargo test -p
ai-platform-sdk-agent-engine` and rustfmt check in the assembled kernel workspace.

## Additional authority fingerprint assignment

Added native `platform-migration-authority/src/fingerprint.rs` and submodules.
The catalog snapshot contains all 28 queries extracted from unchanged Python
fingerprint/fingerprint_catalog source, including separate domain constraints,
plus exact category ordering, logical owner mapping, literal LIKE prefixes,
extension allowlist and local transaction search-path restoration.

Reference rows retain table/column ordering and ASCII-escaped type tags. Scalar
normalization covers booleans, integers, Decimal's default 28-digit normalization,
Python float.hex, strings/JSON text, bytes, UUIDs, date/time/timestamps, enums and
domains. PostgreSQL array text is parsed directly so numeric precision and nested
list shape are retained. Unknown unsupported PostgreSQL types fail closed.
SQL NULL and JSON `null` remain distinct. Added 20 original-Python golden value
cases and catalog inventory/role-normalization tests. No live PostgreSQL
fingerprint comparison has been run in this lane; coordinator owns compilation
and database verification. A passing unit suite alone does not prove full live
catalog parity.

## Additional native configuration/startup assignment

Added `ai-platform-config-loader`, an in-process Rust settings resolver and
explicit JSON/YAML native-deployment reader. Gateway binary now supports
`--config-env` and `--check-config-env` alongside existing stdin modes; no wrapper
child or secret-bearing stdout pipeline. Source defaults/schema and eight
synthetic expected settings fixtures are bundled JSON, with no Python build or
runtime dependency. `.env` plus process environment preserve early source dotenv
loading, nested settings precedence, aliases, typed coercion and validation.

`GATEWAY_NATIVE_CONFIG` is required for the native-only deployment policy. Root
owns Cargo registration/dependency and Docker/Compose wiring. Native loader tests
and gateway compilation are pending the coordinator's serialized gate. Source
fixture generation ran with a cleared process environment, dotenv disabled and
synthetic inputs only; no deployment secrets were read.

Coordinator verification update: root reported the standalone native engine's
actual `cargo test` run passed 12 tests, including the subsequently implemented
read-only, bounded OpenAI SQLite history import. SDK service lane owns that
history import and later composed service/engine integration coverage. Earlier
notes above about an unimplemented OpenAI SQLite import are superseded; opaque
Claude SDK resume remains fenced. Additional tests added afterward require their
own run and are not covered by the 12-test result.

## Live PostgreSQL acceptance (2026-10-15)

On an isolated PostgreSQL 17 loopback-only fixture, all four Rust fingerprints
match the original source implementation exactly. Structural line comparison
also matches all 4,112 lines. This exposed and fixed a compatibility detail:
asyncpg decodes PostgreSQL internal `"char"` catalog values as bytes; the frozen
contract preserves Python bytes representations, including NUL. The structural
hash is the unchanged frozen baseline hash
`1cb97bebe4aef680d61f88b46321ddf097a0404afc906be4f36b846e40140d39`.
The temporary source oracle exists only outside the engineering tree. The
committed acceptance entrypoint is shell plus Rust:
`tests/acceptance-postgres.sh` and `examples/provision-fixture-extensions.rs`.

Initial fresh bootstrap correctly refused missing admin extension ACL hardening
(`no-public-routine-acl`). This was a fixture prerequisite omission; no checks
were relaxed. Full fresh/epoch/startup acceptance with the exact admin helper
remains pending its serialized build and execution.

Fresh fixture follow-up: admin extension hardening and all seven distinct role
connections passed. Fresh bootstrap then correctly refused PostgreSQL 17's new
MAINTAIN ACL privilege against the frozen PostgreSQL 16 baseline. All three
other fingerprints match frozen values exactly. Both original source and Rust
produce the same PG17 ACL digest
`acdeb0d10929a398196a65ed441cdf25073b7cb8897b57560d1f1004abac28fb`.
A diagnostic-only removal of `,MAINTAIN` from 165 source ACL lines reproduces the
frozen ACL digest exactly:
`9f2400d21e7b14617186428ccd65586efa541001d0b2eccba094b4ddc1186674`.
No implementation filters this privilege or relaxes the frozen contract. Full
fresh/epoch/startup acceptance requires the baseline's PostgreSQL 16 environment.

Source oracle snapshot SHA256 values:
906e46fd4301849da23d2d31e156b90ec7b908ff3e99080c91c9a14919a69186  database/authority/fingerprint.py
5e1b680517aba64714222c98815fc0c6c1b469ab1b1ca29754306bacfaf7af68  database/authority/fingerprint_catalog.py
579c1146cad54e9978fcde32fd94d2e89882f77a55b974ed571dce4e2c866d52  database/authority/fingerprint_values.py
74ffe24788479e72840681d9522838a617d874cd3f170f70c647f9c0072eb2b2  database/baselines/2026_08_post_kb_v1/manifest.json

PostgreSQL 16.15 was then built from the official tarball after vendor SHA256
verification, with single-job compilation, isolated prefix, and uuid-ossp,
pgcrypto and pg_trgm extensions. On a new disposable cluster, the Rust authority
passed frozen baseline installation, all 17 epoch migrations, read-only verify,
and idempotent migrate followed by verify. All seven distinct role connection
checks passed. Application startup probes exposed a permission-sensitive
`to_regclass` lookup of legacy public-schema alternatives. The authority owner
is correcting that probe without widening grants; five startup gates are not
yet claimed passing. The isolated cluster was stopped cleanly after each run.
c1575341fa7bd40f5274ea465b34390f4dc64cdd0770af327005caaeb9f6b7ed  postgresql-16.15.tar.bz2

The committed core acceptance shell was separated from the historical startup
CLI probe and rerun on a second empty PostgreSQL 16.15 database. It exited zero:
roles (7), frozen baseline, epochs (17), verify, status, idempotent migrate, and
second verify all passed. This does not imply service admission passed. Gateway
bootstrap currently has no actual schema admission guard (confirmed with its
owner); that missing integration is reported separately for correction.

## Recorded producer and runtime contract migration

Added native `platform-eval::producer` with strict canonical turn normalization,
run/tenant/session binding, ordered one-to-one tool lifecycles, complete budget
snapshot and exhaustion arithmetic, monotonic counters, and durable pause/resume
checkpoint identity plus dual argument-hash encodings. Unsupported plan,
compaction and policy claims remain unknown. Seventeen frozen original-source
artifact cases are embedded in Rust tests; there is no interpreter dependency.

Added the native readonly capability contributor bridge with async trait
contributors, immutable scoped ownership, copied JSON payloads, metadata-only
filters, stable ordering/deduplication, collision rejection, and all source item
projection helpers. Four named Gateway integration targets now exercise V2
receipts, pure control/configuration contracts, original readonly contributor
contracts, and evaluation gate/feedback/export contracts. Eleven source gate
receipts are embedded as static JSON. These targets do not yet cover all original
DB/HTTP orchestration mocks in control-plane and eval trace route tests; passing
these targets alone must not be represented as complete HTTP parity.

Producer verification: coordinator `eval-tests3.log` confirms all five producer
Rust tests passed, including exact comparison of all 17 frozen source artifacts.

## Native parity benchmark collector

`platform-eval::parity_support` and `native_agent_parity_benchmark` now port the
manifest validation, safe fixture loading, neutral scenario prompt generation,
work-product parsing, seven validator families, private receipts, competitor
CLI configuration/session handling, summary acceptance, and version/evidence
receipts. The AI Platform adapter is owned by the coordinator; shared scenario
validation is owned by the evaluation lane. No benchmark/provider execution was
performed as part of this code migration.

The external Hermes and OpenClaw executables remain explicit official-ingress
benchmark dependencies. They are not shipped as the application's implementation.
The `python_reserve` task name describes a generated work product, not a runtime
interpreter dependency: RustPython parses its syntax, and a bounded native
interpreter evaluates the restricted function. Calls/imports/attributes are
screened before evaluation; 10,000 steps and bounded arithmetic stop runaway
candidates. Unsupported Python constructs return a failed runtime receipt
(`NotImplementedError`), rather than invoking an interpreter. This bounded
subset is a disclosed compatibility limit against arbitrary generated Python.

### Native benchmark and recovery harness ports (code-only continuation)

The root's serialized `eval-release-tests4` run passed 52 library tests, including producer frozen source receipts and native parity support / bounded AST reserve evaluation. No external benchmark competitor or model call was executed.

`platform-migration-authority/tests/runtime_thread_store_migration.rs` now ports the original thread-store migration matrix: additive SQL contracts, twice-applied isolated-schema setup, immutable assignment and snapshots/items, scoped idempotent legacy import, concurrent imports, running/blocked and approved-unconsumed fences, paired tools and hashed approvals, source drift and atomic unpaired-tool rollback, twenty concurrent gap-free appends, atomic model-lease reservation/replay/accounting. The nine live test functions are explicitly ignored in ordinary test runs; the dedicated contract shell opts into them. One static source SQL contract test needs no PostgreSQL. Test cleanup runs after assertion panic as well as returned errors. This continuation did not execute PostgreSQL.

Native authority binaries `runtime_recovery_reference`, `runtime_recovery_observe`, and `runtime_durable_recovery_gate` replace the three matching harness scripts. The reference command retains its synthetic database and refuses replacement of differing epoch 3 evidence (epoch 4 replacement requires the original explicit flag). Observation projects only the original selected non-secret fields and active-run core call/output counters. The durable gate preserves Compose-owner checks, local E2E login, six controlled crash stages, cancel/revoke/reject/unknown-worker cases, identity and exactly-once assertions, original-run recovery, and harness-file cleanup. These are explicit operator commands and have not been run. Failure output sanitizes driver/HTTP details; Rust errors use a native failure type rather than a Python exception class. Compilation and static tests are pending the root's serialized build.

The V2 observation validator is now a self-contained shared Rust source module in `platform-eval/src/runtime_contract.rs`; Gateway reexports this module through a source path without introducing the heavy eval dependency. The `agent-runtime-eval-contract` binary accepts `{thread,events}` JSON via stdin or `--input`, prints `{passed,failures}`, and exits nonzero for invalid observations. This preserves the original failure ordering, aliases, lifecycle pairings, compaction receipts, cancellation closure and visible-output requirements, including source primitive coercions (boolean sequences are integers, numeric identity/output and retention IDs are stringified). Seven original source cases and three coercion/failure-order tests cover both public callers. Pending serialized compile; no external service operations.

### Studio source test migration in progress

Native target files now exist for `agent_studio_eval`, `agent_studio_database`, `agent_studio_publication`, `agent_studio_rbac`, `agent_studio_channel`, `agent_studio_runtime`, and `agent_studio_observability`. The two release-evaluator source test files are ported across eleven pure tests. Concrete PostgreSQL tests cover the first domain migration matrices (idempotent/additive DDL, all ten composite child foreign keys, immutable sealed versions and publication events, last-owner and principal/owner constraints), repository role and cross-tenant isolation, publication lifecycle DDL, and operations audit redaction/dimensions, quota observations/alerts and durable legal-hold blocking. Source SQL fixtures are static SQL, not interpreted source. Tests create only random schemas or a disposable publication database and clean up on assertion failures. No PostgreSQL or HTTP tests have been executed in this code-only phase.

Coverage is deliberately not overstated: these targets are partial for the broader roughly nine-thousand-line assigned Studio suite. The full publication transaction races, runtime HTTP/session/attachment/token behavior, HTTP analytics filters, copy-route permissions, and complete deletion/interrupted-cleanup races are not yet ported. Native `AgentState` has concrete auth/control/session/source/trace/public collaborators and no existing lightweight test graph, so pure helper tests are not claimed as HTTP parity. The regression manifest must not treat target existence as equivalent completion of those source groups. Live database cases are ignored in ordinary compile/test runs; an explicit gate must use `--include-ignored` and fail when its isolated database prerequisites are unavailable.

### Studio retained integration assertions (2026-10-15 12:12 UTC, code only)

Added a test-only `agent_studio_support/state.rs` composition of the production AgentState, AdmissionService, RuntimeControlPlane, source access, session manager, trace repository and PublicRuntime constructors. Explicit loopback Redis and disposable PostgreSQL are required; ordinary test runs ignore these integration cases, and the explicit Studio gate must include ignored tests. The separate trace control pool is closed on errors and assertion panics. No infrastructure was started or these integration cases executed during this addition.

Retained behavior now authored against native production methods/HTTP dispatch:
- Publication lifecycle queued/running/cancelled/terminal and immutable evaluation/event/request rows.
- Publication replay after draft changes, conflicting payload rejection, event-insert failure rolling back every version/publication/event/request, and concurrent same-tenant keys across different agents.
- Stale draft and changed evaluation dataset fail without release writes; dataset authority is tenant bound. Disabled/unauthorized models/providers and missing runtime provider readiness reject release.
- Rollback replay, revocation revalidation and preservation of current pointer and release row counts on rejection.
- Actual embed HTTP dispatch: exact origins, missing/wrong/wildcard parent rejection, Referer acceptance, CSP/security headers and no reusable API credential/internal snapshot leakage.
- Embed signature, publication, origin, browser cookie, stable abuse principal and expiry validation (expired claim signed using the isolated test key; no production clock override).
- API token publication/scope/version isolation, expiry/revocation, scoped principal identity and last-used update; hosted private/tenant authorization and immediate publication disable.

Language-specific private callback injection and mocked Python object call-count expectations are not retained as requirements where Rust production constructors provide mandatory typed collaborators. Their observable fail-closed/replay/atomicity assertions remain required. Still incomplete: complete authenticated management HTTP token CRUD and publish routing matrices; downstream turn/resume/SSE/attachment receipts; all live-run promotion and concurrent dataset/model rollback race variants; deletion/analytics HTTP matrices. No claim of complete Studio behavioral coverage follows from the workspace compiling or the pure tests passing.

Additional authored HTTP cases at 12:16 UTC use the actual Axum router and native JWT/Redis/PgAccounts admission: server-owned evaluation fields, owner evaluation execution/list/cancellation, publish key requirement and replay, viewer denial, and token CRUD/rotation/hash-at-rest/expiry. The token management HTTP gap listed above is now covered in code (still not executed). Found and fixed the source-required missing release Idempotency-Key status from 400 to 428 in publish and rollback, narrowly coordinated with the gateway owner. Other listed downstream/race/observability gaps remain.
