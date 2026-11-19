# Native operational tools migration

Implemented `rust/platform-native/crates/platform-tools`, binary `platform-tools`.
No command invokes Python or imports historical Python modules. Parent coordinates
workspace registration, callsite replacement and serialized compilation.

## Implemented

- Harness document/budget/link/workspace/Make/CI/runbook validation.
- Cargo service-boundary validation, including target/dev/build dependencies.
  One explicit dated Knowledge → Gateway library reuse exception is validated and
  reported separately. It is not a claim of fully separated library contexts.
- Forbidden Python source/operational reference inventory and zero-source gate.
- Source-lock validation; immutable Docker image pair/platform/label verification
  using only `docker image inspect`; source bundle/overlay identity hashing.
- Source-candidate validation and assembly with payload hashes/modes, reviewed
  Git blob preimages, compiler proof and full composed source inventory checks.
- Recovery ZIP exact member inventory, SHA-256/size/CRC, bounded decompression,
  unsafe/duplicate/symlink refusal, optional zero-Python project source check.
- Topology modes, deep compose merge, exact service IDs and replicas.
- JSON dotted-path readers, file hashing, expanded/non-strict resolved paths.
- Embed Nginx/Helm/native Gateway configuration checks and native TCP fixture,
  including GET CSP/no-store and original HEAD 501 behavior.
- Affected gate selection: exact original glob semantics, changed + untracked
  paths, fail-closed unmatched paths, existing evidence JSON and error exit 2.
- Final CI enforcement: actual executable outcome-preserving entrypoints,
  declared job dependencies/full history checkout, selected gate/path coverage,
  all upstream statuses successful, only selector satisfied by current final job.
- Hosted-only changed Rust crate execution for native and composed overlay
  workspaces; exact pinned toolchain/overlay/preimages, locked tests, single job,
  55-minute timeout. Requires Unix coreutils `timeout`, Git and pinned Cargo.
  Lock changes conservatively select the overlay workspace; the previous narrow
  receipt-only optimization is intentionally not used as a proof shortcut.

## Evidence

Root's `tools-tests2.log` records 12 tests passed with zero failures, including
real ZIP fixture verification/tampering, path/symlink/loop resolution, embed HEAD
behavior, exact glob/overlap selection and outcome-preserving shell recognition.
CI enforcement and changed-crate tests were added after that snapshot and await
root's next serialized test. No Docker image validation or actual changed-crate
Cargo execution is claimed from local unit evidence.

## Still open

Historical specialized evaluation/live stack scripts, release receipt generation,
image recording, resource locks and repository known-candidate repair mappings are not yet
all ported. Unknown commands fail; they do not provide fabricated green evidence.
Historical Python source still exists during migration and zero-source gate is
expected to fail until parent completes removal. The tools do not relabel old
source locks as valid after source edits. Native operations are not release proof.

## Subsequent completed ports

Durable `evidence validate|status|cleanup` now ports policy/schema/provenance/
manifest/oracle checks and reversible quarantine. Explicit exact authorization,
external existing quarantine, age/tracked/reference/mount/worktree restrictions
are preserved; symlink ancestors are additionally refused before candidate
contents are read. New run directories and a fresh pre-move hash eligibility
check prevent accidental overwrite/stale approved bytes. No cleanup was run.

Live `topology-inventory` and `single-instance-guard` are implemented, including
six current Compose inputs and Helm values/templates. Source identity is actual
Git HEAD or explicit supplied source SHA for a recovered archive. Native
Knowledge route facts are explicitly static Rust route registrations; historical
Python decorator metrics are not fabricated. The parent subsequently compiled and passed the 23-test tools snapshot, including evidence, singleton, LOC, CI and changed-crate tests. AST hygiene was added after this snapshot.
Release orchestration moved to the parent's separate platform-release scope.

Native LOC requires an explicit Rust oversized-file ledger, with backend800 and
TypeScript500 thresholds; old Python ledger cannot silently bless native growth.
Real Git ancestry/blob proof remains mandatory, so a restored source-only tree
without Git objects cannot claim this provenance gate passed.

`hygiene-check` now uses syn2 full AST parsing for Rust test functions, preserves
TypeScript checks, exact exception ownership/expiry/staleness and skip baseline
multiset matching. Rust ignore markers absent from a native baseline have no
implicit exemptions. Python syntax/AST inspection is replaced by genuine native
AST inspection under the requested language migration. No interpreter is invoked.

`known-candidates` now checks the exact four source candidate identities and
native repair mappings. Gateway supplied real loopback transport tests for
504/502 and semaphore/connection release. Gate inspection uses native AST and
called local assertion helpers; execution evidence is still required separately.

`resource-lock` now ports shared Git-common-dir lock ownership, atomic fsynced
redacted receipts, heartbeat, bounded runtime identity, explicit force-release,
same-host dead-PID elapsed-timeout stale proof and real child process-group
cleanup. Unknown cleanup retains ownership. Added tests for actual exclusion,
stale reap, redaction, unsafe entries and real native child termination; awaiting
root's serialized test of this expanded snapshot. No production lock was changed.

Latest root evidence `tools-release-authority-tests.log`: all 34 platform-tools
tests passed, including real process-group termination, mutex exclusion, stale
reap, syn AST hygiene and native candidate checks. Subsequent build-wrapper
helpers still need the next serialized compile/test. All four assigned
`scripts/harness/build_*.sh` now contain zero Python references and passed bash
syntax checks. No image build/probe/recording was executed during this port.
Gateway preflight belongs to the parent's platform-release binary; a temporary
duplicate tools implementation was removed to keep one maintained implementation.

### Additional operational ports

`platform-tools source-lock refresh-source-lock` rebuilds lock identities from a verified source receipt and invalidates all image references. `refresh-overlay --cargo-workspace DIR [--cargo-metadata FILE]` preserves exact Cargo graph closure/SBOM references, URL-namespace UUIDv5 identity, source/overlay hashes and image invalidation. Offline metadata requires its matching exported Cargo.lock. These are source evidence updates, never substitutes for build/test evidence.

All four `scripts/harness/build_*.sh` wrappers now call native source identity, bounded initialize-probe and CLI material operations. Runtime smoke payload generation, both capability-worker smoke phases, and rollback env/image/API-fingerprint/report helpers are native Rust. HTTP probes disable proxies and redirects, bound responses and preserve original status, cursor, cancel, replay and recovery assertions. Shell syntax was checked; Docker and live API probes were not run. The thread-store wrapper uses the native authority test target ported by the runtime owner.

These additions still require the parent's serialized compilation/test pass. No claims of live provider acceptance or successful operational rehearsal follow from source migration alone.

### Final harness source ports (pending current compile)

- `source-lock generate` and `generate-composed`: real Git/Cargo identities and verified offline composed export receipts; no image or compile acceptance is inferred.
- `package-platformctl` and `package-desktop`: target matrix/build environment, ELF/Mach-O/PE machine checks, source-material Git cleanliness, real bounded offline executable checks, deterministic zip/tar.gz, desktop extracted executable/identity validation, source recheck, checksum and unsigned receipts. Packaging material identity explicitly names the new Rust tool implementation instead of removed interpreter scripts.
- `native-candidate-plan`: source markers and exact plan-only Compose command inventory.
- `agent-runtime-readonly-gate`: executes the production bridge integration test `readonly_contributor_gate`; `agent-runtime-write-gate`: assembles controlled upstream plus overlay and executes the runtime lifecycle library tests; `agent-runtime-single-kernel-gate`: syn call-target checks on native gateway handlers and unconditional singleton Compose configuration.
- `agent-runtime-text-gate` is a standalone Gateway crate binary using actual control/model planes, production PostgreSQL source reader, signed leases, private model-plane router, isolated PostgreSQL/Runtime, exact four text scenarios, ordered one-terminal closure, TTFT, billing and restart-memory assertions. The isolated base table is exposed via the canonical assistant.sessions view. All actual document-reference authorization is denied for this text-only scenario. No container/provider execution was performed during migration.
- `gateway-performance-baseline`: CLI remains offline plan-only. Library `ai_platform_tools::performance` ports the actual bounded fixed-GET HTTP/1.1 client, warmup, concurrency/deadline engine, exact-PID CPU/RSS/FD/socket sampling, quantile/error summaries, and explicit cancellation partial observations. Measurement is exploratory, never an enterprise SLO or release acceptance claim.

Recovery trio is provided by the migration-authority owner; KB boundary/release evidence by platform-openapi; runtime observation contract by platform-eval. Those delegated modules need their own compiled evidence before final acceptance.

Wire fingerprint helpers explicitly sort nested objects and match Python float formatting despite workspace serde_json preserve_order feature unification. Added tests cover this boundary, graph closure determinism/collisions, image invalidation, dotenv escapes, binary architectures, deterministic archives, safety budgets, response framing, absent measurements, and stale PID rejection. Compilation and tests remain parent-serialized.

Deployment contract migration: `cargo test -p ai-platform-tools --test deployment_script_contracts`
now covers the four former script suites with Rust-owned fixtures, subprocess
assertions, static contracts and exact native authority/topology invocations.
Only Python implementation-copy checks become compiled-image/release-unit checks.
Fake Docker/curl/authority executables isolate every runtime interaction; these
are shell contract tests, not a Docker deployment or paid-provider acceptance.
Performance process CPU measurement fails closed if getrusage cannot measure it;
it no longer substitutes a fabricated zero.

Shared distributed HTTP tracing closure: `platform-gateway-foundation::distributed_tracing`
now implements the common source OTel SDK bootstrap and inbound middleware,
reused by Gateway and Knowledge through the Gateway facade. Explicit service
startup precedes pool construction; shutdown flushes on a blocking worker before
Tokio teardown. Optional OTLP/gRPC uses the existing 0.31 SDK bounded batch
processor; unset endpoint keeps valid in-process W3C context without exporting.
Incoming sampled/unsampled parent and tracestate survive; SDK spans have SERVER
kind, source HTTP attributes and 5xx error status. The inbound adapter returns
response headers without polling or buffering SSE bodies. Gateway's separate
StreamingTracing response-ID contract is retained; its internal correlation
bridge now receives the actual SDK carrier. Local-only tests cover parent
sampling, tracestate and a never-ready streaming body. Automatic asyncpg query
instrumentation and HTTPX CLIENT-span monkeypatching do not exist in these Rust
drivers and are not claimed by this server-span migration.

Make/CI native wiring: all eval, unit, packaging, image-lock, runtime-contract
and isolation recipes now invoke their actual Rust modules/test targets. The
native live OpenAPI command ports the original path-superset and retained-required
field assertions, including explicit SKIPPED receipts for an unavailable stack.
Knowledge's explicit live Qdrant gate invokes both ignored production-API tests;
ordinary unit runs cannot claim that live proof. KB migration recipe remains
pending its substantive native full-chain/restore/concurrency test port, with no
substitution of the much narrower general authority acceptance smoke.
Publish CLI now bootstraps native tools before using them; harness triggers name
actual Rust source/test paths and Gateway unit CI captures its declared log.

KB migration recipe closure: the named gate now executes all five ignored
`kb_migration_contract` tests with one test thread. CI grants CREATE DATABASE
only to its disposable hosted PostgreSQL fixture via the suite's explicit
`KB_MIGRATION_TEST_ALLOW_CREATE_DATABASE=1` and `KB_MIGRATION_TEST_ADMIN_DSN`
contract and installs pg_dump/pg_restore. Consolidated coverage is full-chain
ledger/role/FK/idempotence, 97/112, lock serialization, TERM/KILL rollback and
101 dump/restore old/new writer boundaries. Specialized historical embedding and
artifact assertion suites remain unverified obligations; this target is not
represented as a rerun of all original 18 source files. No DB test was run here.
