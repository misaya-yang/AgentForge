# Full Rust migration status

Updated: 2026-10-15 14:22 UTC. This checkpoint supersedes earlier in-progress
checkpoints in this document; the underlying historical evidence remains retained.

## Authorized scope

The final deliverable must contain no Python implementation, operational or test
source, and no Python runtime dependency. Public behavior, authorization boundaries,
Web UI contracts, database ownership and durable evidence remain requirements.

At **12:45 UTC on 2026-10-15**, the owner explicitly authorized a Rust-native
architecture upgrade. This supersedes the earlier requirement to mechanically
retain the Python architecture. Axum/Tokio/Tower governance, typed internal gRPC,
WebSocket lifecycle management and durable Redis Streams are now deliberate native
implementation choices. Historical behavior remains the compatibility reference;
the upgrade is not permission to remove functionality or weaken acceptance gates.

The **11:05 UTC code-first execution limit remains in force**: implementation,
necessary compilation and targeted tests are authorized. Further live database,
provider, browser/deployment acceptance and heavyweight model downloads are paused.
Earlier isolated database results are historical evidence, not a new live run.

## Current native architecture

- **Gateway:** `gateway --config-env` supplies public HTTP/API routes and typed
  configuration. Public route and identity contracts remain in place.
- **Knowledge:** `platform-knowledge-service` owns independent API/worker roles.
  Internal typed Knowledge gRPC calls enter the same signed in-process HTTP router,
  retaining actor validation, replay protection and resource authorization. The
  public HTTP facade remains available. Qdrant uses its native client transport.
- **HTTP and streaming governance:** shared Tower/Axum admission holds a concurrency
  permit through response-body lifetime. Header deadlines and body-idle deadlines
  are separate; active SSE has no whole-response lifetime cutoff. Request cancellation
  reaches existing dispatcher/capacity cleanup. Dashboard WebSockets retain their
  own active-session budget after HTTP 101, with bounded send and idle behavior.
- **Redis Streams:** the actual GenericREST MQ transport uses atomic fenced
  retry/dead-letter settlement, bounded reclaim/admission and graceful drain.
  Delivery is at least once; business effects still require durable idempotency.
  Registry/adapter shutdown runs before shared database/Redis resources close.
- **SDK execution:** `sdk-agent-worker serve` invokes the native
  `ai-platform-sdk-agent-engine` Responses/Messages execution loop. This is not the
  official Python SDK implementation. Scoped OpenAI SQLite history import is
  read-only; opaque Claude SDK session state is not native conversation history.
- **Database authority:** `database-authority` retains the existing baseline,
  epochs, legacy ledger, advisory lock, roles and fingerprints. No replacement
  ledger or compatibility-grant widening is implied.
- **Clients and tools:** native CLI/desktop/client and local-node components,
  `platform-tools`, native evaluation and release/recovery tooling replace their
  implementation counterparts. The Web console remains TypeScript.

Main-workspace resolved pins checked in batch41 include Rust 1.99.0, Axum 0.8.9,
Tokio 1.53.2, Tower 0.5.3, Reqwest 0.13.5, Redis 1.7.1, Tokio-Tungstenite 0.30.0,
Tonic 0.14.6, Prost 0.14.4 and Qdrant-client 1.19.0. Cargo.lock is authoritative,
including transitive versions. Independent Runtime/SDK workspaces retain their
separate pins. WSS selects an explicit crypto provider and verified WebPKI roots;
the Reqwest upgrade retains certificate/hostname verification with platform trust.

## Current verified checkpoint

All builds were root-serialized with pinned tools, no debug/incremental output and
external Cargo artifacts. These are compilation and targeted native contracts,
**not integrated production or live-provider acceptance**.

| Completed evidence | Result and scope |
| --- | --- |
| `workspace-check41.log` | Main workspace all-target check passed with the final main-workspace dependency pins. |
| `knowledge-rpc42.log` | 4 passed, including actual loopback HTTP/2 transport through the signed Knowledge router and replay protection. |
| `foundation-native43.log` | Adapter shutdown 1; multibyte codec oracle 1 covering 1,190 cases; HTTP pool lifecycle 1; MQ 15; request governance 8; JSON WebSocket transport 2. All passed. |
| `gateway-native45.log` | Actual authorization/router target 10; served native OpenAPI shared-producer target 1; judge-label normalization regression 1. All passed. Authorization includes dashboard WebSocket post-upgrade capacity and idle/disconnect cleanup. |

The earlier MQ drain failure in batch37 was corrected and its expanded target
passed in batch43. The earlier authorization fixture fallback-merge failure was
corrected to match production router composition. Disk guards stopped some build
attempts, including gateway batch44; stopped attempts are not test passes. Batch45
provides the subsequent Gateway test results. No test or build is described as
still running at this checkpoint.

### Retained earlier evidence

These earlier results remain useful for their stated snapshots and scopes; they
are not automatically full retests of every later native change:

- SDK service 13 and native SDK engine 12 tests passed. Actual native service/child
  engine with loopback provider/authority covered streaming, receipt replay,
  history, approval allow/deny, disconnect cancellation, deletion and shutdown.
- Database authority initial 27 tests passed. Earlier isolated PostgreSQL 16
  acceptance covered fresh baseline, all 17 epochs, verification and idempotent
  replay. Existing application/CLI role-window incompatibilities remain explicit.
- Client 35+11+3, Local Node 5+2+8 and config loader 6 tests passed at their earlier
  checkpoints. macOS GUI/Keychain live behavior remains unverified.
- Native eval fixture tests passed (15 library, 4 golden, 2 RAG). Eval online
  sampling/outbox 11, plugin parsing/manifest/components 18, and boot/readiness 2
  passed. Full disposable PostgreSQL/Redis boot remains explicitly ignored.
- Earlier release/evidence tools passed 8 and operations tools passed 34 targeted
  tests. Later bounded deployment contracts passed 43 and client streaming 12.
  None of those test totals substitutes for deployment acceptance.

## Source retirement and remaining boundaries

**Current deliverable is a native source checkpoint, not full acceptance.**
The coordinator recoverably archived825 Python production/utility sources with
per-file hashes. At the owner’s2026-10-15 15:45 UTC direction, the original691
legacy test/reference files are now accounted for as37 mapped behavioral suites,
22 namespace-only markers and632 explicitly unported/partial recovery references
isolated outside the main tree. The main tree has no Python source files; that
physical property does not establish full behavioral/test migration. See
`reports/unported-test-recovery-inventory.json` and the current
[Codex handoff](../handoff/RUST_SOURCE_CHECKPOINT_20261015.md).
Ignored database/restore/concurrency tests remain unrun.

The Rust code-interpreter supervisor still offers an optional external
**Python-language sandbox** with Python ecosystem packages. Whether to retain that
capability conflicts with literal zero-Python runtime requirements and awaits the
owner's decision. No VM substitute or silent feature removal has been made.
Arbitrary external in-process Python LangGraph modules similarly require native
plugin implementations; none is shipped/configured in this repository.

The explicit native-v2 coding release manifest and native validator identities are
implemented; historical v1 artifacts remain immutable. This is not proof that a
coding sandbox or dual-provider judge acceptance run passed. Native HTTP pooling
and timeout semantics, and supported charset boundaries, are documented rather
than claimed identical to every external Python codec/transport extension.

Immediate delivery work is final compilation/targeted testing, source/runtime
identity refresh and stable archive verification. Unported test work and business
defects continue after the source checkpoint. The external sandbox decision and
later owner-authorized integrated acceptance remain open. Compilation, route counts and retirement
maps alone do not establish release completion. No completed zero-Python recovery
archive or deployment is claimed here.

## Native development and recovery

`make native-install` builds native service and operational executables in an explicit
external Cargo target and installs to `NATIVE_BIN_DIR`. Development setup fetches
locked native manifests rather than installing a Python workspace. Compose binds
`GATEWAY_NATIVE_CONFIG_FILE` read-only; checked-in examples contain placeholders,
not deployment credentials or permission to deploy.

`prepare_native_gateway_config` converts explicit environment/native JSON inputs to
a private resolved document without dotenv discovery. `recovery-pack` creates a
stable source-only ZIP only after its checks pass: it refuses `.py`/`.pyw`, checks
inventory/content drift, excludes credentials/data/build caches, scans secrets and
rereads every member for hashes/CRC. It does not upload or replace Library files.

The opt-in staged secret guard is documented in
[`.githooks/README.md`](../../.githooks/README.md). Hook installation is a local Git
configuration choice; no hooksPath installation is implied by this migration.

## Additional closure evidence (2026-10-15 14:06 UTC)

- Standalone SDK engine lock refreshed for reqwest 0.13.5; final explicit-retry-policy build/test56 passed 13 tests.
- Client streaming test53 passed 13 tests with all production client builders explicitly disabling implicit protocol retries; application GET resume remains enabled.
- RPC test50 passed four unit tests and one actual HTTP/2 channel cancellation/load-shedding test.
- Foundation test49 passed real RESP-wire fencing/reclaim and explicit WebSocket TLS-provider construction; evaluation native-v2 manifest/policy consistency also passed.
- Runtime overlay source-lock refresh54, overlay identity verification and lock validation passed. The lock records local_source_locked and no runnable images; upstream Git preimages were not verified from the tar extraction.
- Architecture gate58 passed with the identity-checked composed upstream workspace and the one explicit transitional Knowledge-to-Gateway library exception. Resolver negative fixtures are being rerun after correcting a missing fixture directory.
- Native image inputs lock records 39 input files, reread successfully; image execution is unverified.
- Legacy test map is a coverage audit, not a completion percentage. Unknown assertion equivalence remains explicit and no remaining legacy test files have been retired solely to satisfy a zero-source scan.

## Final architecture checkpoint

Workspace all-target check60 passed after explicit Reqwest retry policies and client-helper relocation. Native source-lock refresh63 and reviewed hygiene66 passed; hygiene uses a new native ledger and preserves the historical ledger. The 65 named Rust skips and seven TypeScript conditional markers are documented prerequisites, not passed integration tests. The Runtime quiz fixture is accurately labeled connection smoke only.

A finite remaining-test audit records 691 legacy Python test/reference files: 82 partial mappings, 574 unverified assertion-equivalence suites and 35 framework/fixture reviews. These counts describe audit evidence, not production implementation progress. Existing native tests can cover additional behavior; unmatched entries do not establish missing production code. They also cannot be used as proof of full migration. That initial audit is historical; subsequent verified, fully mapped suites are retired recoverably through the retirement manifest. Unmapped references remain.

Additional runnable Rust regressions now cover JWT cryptography, plugin symlink/limit aliases, authority provenance/role credentials, Runtime assignment and Confluence control/revocation. Live database variants remain explicitly ignored under the current execution scope. Targeted offline execution is recorded separately.

## Source-equivalence checkpoint (2026-10-15 15:37 UTC)

Verified native replacements now include local action authority/ledger/pairing, RAG and stateful evaluation gates, typed endpoint and security helpers, tool gate parsing, and SDK worker frame/lifecycle behavior. The evidence directory records terminal logs77–84; each report distinguishes controlled fixtures, actual loopback transports and unexecuted external-stack tests.

Source-backed repairs include single-consumption Knowledge capabilities in the SDK loop, safe legacy local-ledger readability, duplicate YAML rejection, provenance component ordering, and malformed event error handling. Read-only current-user introspection, frame-aware SSE heartbeats, event registries and authorization-to-cancellation fences are in the next serialized verification batch. These recent changes postdate workspace check60; a fresh full check is still required.

Source identity locks and native image-input hashes will be refreshed after these batches stabilize. No updated ZIP or runnable image has been delivered.

## ZIP-first checkpoint (2026-10-15 15:54 UTC)

The owner requested delivery before further defect repair. Whole native workspace/all-target compilation96 passed after correcting two test Option accesses; warnings remain. Runtime source identities94 (148 overlay files), native image inputs94 (39 files), harness97 and architecture97 passed. Images remain unbuilt/unaccepted. Latest SDK response ownership tests92 and eval producer/finance/CRA/runtime-contract tests95 passed. Selected frozen Gateway regressions are running before packaging; later test failures will be listed, not hidden or used to restart broad bug fixing.

See `reports/native-checkpoint-verification.json` for terminal evidence and `docs/handoff/RUST_SOURCE_CHECKPOINT_20261015.md` for deferred work. The physical zero-Python source property excludes632 retained external unported/partial test references; it is not full legacy assertion equivalence.

Final frozen-source check103 passed. Gateway98 selected tests passed38 with11external-stack cases ignored. Knowledge100/102 passed78 across14targets, with the two tools cases requiring a16MiB debug test stack; the initial default-stack abort remains recorded. Actual source/operational inventory and reviewed hygiene101 passed. This checkpoint is ready for immutable native ZIP capture; upload receipt and ZIP hash are stored externally rather than embedded self-referentially.

## Verified follow-up checkpoint (2026-10-15 16:26 UTC)

After delivering version126, two bounded issues were repaired. Billing quarantine metadata is now monotonic under receipt conflicts (six pure production tests passed; new actual SQL test remains unrun). Knowledge retrieval uses a single heap-pinned inner future; public future sizes fell from about311KiB to1.5–1.9KiB, and the original tools tests now pass on the default stack. Final whole-workspace/all-target check112, reviewed hygiene79Rust/7TS and zero-source/operational-reference inventory passed. Offline coding-fixture expected-failure/reference-pass proof also completed. The current handoff and post-checkpoint evidence describe exact limits; no live acceptance is claimed.
