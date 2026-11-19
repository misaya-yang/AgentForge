# Foundation migration implementation

## Implemented

- GenericREST JSONPath recursive descent now uses depth-first traversal, retaining object insertion order, array order, repeated matches, and scalar slice coercion. The parser retains jsonpath-ng's explicit-dot versus implicit-bracket precedence.
- GenericREST HTTP responses now parse original UTF-8/16/32 JSON bytes independently of Content-Type charset, with BOM/autodetection. Non-JSON text uses declared encoding/replacement and preserves Python's raw-text fallback when original-byte JSON parsing fails. ISO-8859-1 and ASCII explicitly avoid WHATWG alias differences.
- No new dependencies, routes, state stores, auth policies, or Python removals.

## Verification actually performed

- Captured 12 cases from installed jsonpath-ng 1.8.0, including recursive unions, wildcards, duplicates, child/bracket paths, negative-step slices, and evaluation failure.
- Added Rust integration tests `generic_rest_recursive_descent` and `generic_rest_response_bytes`.
- rustfmt 1.99 completed on changed Rust source/test files.
- Cargo builds/tests NOT run by this worker; parent owns serialized execution. No PostgreSQL/Redis/provider/live HTTP acceptance run.

## Initial remaining findings (updated after follow-on implementation)

- Grouping, parent/this, unions, where/wherenot, numeric fields and string indexing were subsequently implemented and their 55-case static source matrix passed. Intersection remains an evaluation error because the pinned source itself raises NotImplementedError. Arbitrary Python objects and non-finite JSON remain outside native JSON representation.
- The later codec port adds exact UTF and 66 single-byte codec tables/280 aliases. Remaining multibyte codec edge cases are not exhaustively proven. Independent httpx write timeout and global idle-pool parity remain documented transport differences.
- Initial pass did not change auth/integrations. Later sections document MQ transport and other production fixes; a whole-program parity claim is still not inferred from counterpart presence.
- Python remains the behavior/deployment reference pending complete native acceptance.

## Database authority follow-on

At the parent's request, this worker also implemented the new
`ai-platform-migration-authority` crate's runtime, canonical ledger SQL,
role-specific credentials, lexical transaction guard, bootstrap/role validation,
fresh installation, epoch application, read-only startup/status/verify/fingerprint
commands, explicit ACLv1 administration, and CLI. The parent owns immutable
manifest and legacy/numeric reconciliation modules; the runtime worker owns exact
fingerprint computation.

The implementation reuses advisory lock (1095781959, 1), all three existing ledger
tables, immutable baseline/epoch artifacts, role prefix rules, and exact ACLv1
receipt/grant matrix. It does not introduce a parallel ledger. Fresh installation
retains an outer transaction and writes its adoption marker only after exact
four-class fingerprint proof. Status/verify/startup use read-only connections;
verify also checks each applied epoch postcondition. Explicit principal DSNs have
no shared/default fallback and driver error text is not printed.

The nontransactional state machine is also ported: registered recovery callbacks,
source digest/checksum binding, 15-minute lease expiry, exact checkpoints, fenced
updates, and atomic success/terminal ledger commit. The CLI supplies no handlers,
matching Python; all current 17 canonical changes are transactional. Cross-runner
open attempts fail closed on the different executable digest. Cancellation of an
in-flight nontransactional library future needs additional validation before that
library path is accepted; CLI cannot exercise it with its empty handler registry.

Added tests cover SQL lexical boundaries, unsafe role prefixes, the exact Python
ACLv1 receipt bytes, checkpoint splitting, immutable canonical baseline/epoch
verification binding, and rejection of unknown/tampered/noncontiguous history.
Rustfmt succeeded. Cargo tests and PostgreSQL acceptance remain parent-owned and
were not run by this worker. No migration was applied and no credentials were read.

## Database operational tools (native ports)

Implemented the following replacement executables in `ai-platform-migration-authority`:

| Historical helper | Native command |
|---|---|
| `python -m database.authority` | `database-authority` |
| `scripts/database/check_successor_proposals.py` | `check-successor-proposals` |
| `scripts/database/render_baseline_contract.py` | `render-baseline-contract` |
| `scripts/database/freeze_baseline.py` | `freeze-baseline` |
| `scripts/database/probe_successor_roles.py` | `probe-successor-roles` |
| `scripts/inventory/database_policy.py` | `database-policy` |
| `scripts/inventory/generate_database_grants.py` | `generate-database-grants` |

Freezer now uses direct native policy/grant APIs; there is no external Python or
unimplemented policy-provider subprocess. It retains the clean pending commit,
explicit distinct scratch names, PostgreSQL/pg_dump 16 requirements, role-DSN
separation, secret-safe pg_dump environment, reference rows, staged fresh-install
reproduction, actual-role connection verification, and manifest-last publication.
Newly generated manifests identify the Rust generator; existing frozen generator
identities and bytes were not changed. Publication stages on the destination
filesystem before renaming, avoiding cross-device rename failures.

Database policy scans native Rust SQL string literals and the same schema/migration
SQL inputs. Former table/domain ownership data is embedded as a JSON artifact.
The native source paths are mapped to their service principals; this is static
evidence, not a substitute for live ACL/role proof. Historical Python-only source
access evidence cannot remain byte-identical after source migration. Dynamic SQL
and ambiguous shared-crate service ownership remain limitations of static scanning
and require review before a new freeze. Existing baseline policy/grants SQL are
only read as immutable fixtures. Grants generation validates source reader/writer
coverage and function-mediated write evidence, rejects unresolved ownership, and
has an exact frozen SQL byte-equality test.

Successor role probe defaults to offline source validation without reading DSNs.
Its explicit opt-in branch retains disposable/loopback checks, actual login
identity and denied permissions, all 22 generation requeue cases, composite FK
negative, progress event payload, durable claim fencing, and separately gated
hostile temporary object checks. DML is rolled back and sequence increments are
explicitly reported as potentially remaining. This port has not been run against
a successor database here.

Validation for these newly added tools: standalone rustfmt succeeded. Parent owns
serialized Cargo build/test and live scratch acceptance; no Cargo or database
operations were executed by this worker. The live worker's early fresh-install
failure was a real `no-public-routine-acl` failed check before admin extension
hardening; verification was not weakened. A safe check-name diagnostic was added.

### Observed parent verification results

Read the parent-produced logs (no Cargo invocation in this worker):
- `full-rust-20261015/foundation-tests.log`: recursive descent 2/2 and response
  byte decoding 3/3 passed.
- `full-rust-20261015/authority-tests2.log`: earlier authority snapshot passed
  27 unit tests, plus main/doc-test targets. This predates the added source-tool
  modules and must not be counted as their compile/test result.
- Live fingerprint worker established all four native fingerprints agree with
  the original source oracle on an isolated PostgreSQL 17 fixture. Frozen PG16
  equality correctly fails only on PG17's added `MAINTAIN` ACL privilege; no
  filtering or baseline rewrite was introduced to bypass that difference.

Live worker subsequently reported PostgreSQL 16.15: fresh install, all 17 canonical
epochs, idempotent migrate, and verify passed. The authority CLI's application
`startup-check` still fails under least-privilege application roles because the
historical helper first resolves `public.*` and then directly reads ledger tables;
its Gateway/Runtime window is also 2–10 by the original constants. This is the
ported source behavior, distinct from native application admission guards. Parent
was notified before changing that contract; no role privileges were broadened.

Latest observed parent log `full-rust-20261015/authority-tests4.log`: 39/39 unit
tests passed, and all seven command targets plus both examples compiled/passed
their test harnesses. This includes exact frozen grants/cutover/verify SQL
regeneration, source-tool guards, URI/reference literal handling, and native SQL
role attribution tests. An additional end-to-end synthetic native policy-builder
fixture was added afterward and is awaiting the next serialized parent test run.
Parent explicitly retained the source CLI startup-check limitation; native
application admission guards are tested separately.

Follow-on parity closure: pending/unavailable baseline migration now uses the
historical tracked legacy/per-service chain only, rejecting ledgerless databases
and adopted markers without their frozen source; fresh install still requires a
complete frozen baseline. Legacy status output now reports the original filename,
numeric and per-service row counts plus pending-file count instead of a new JSON
format. Added baseline readiness transition tests.

`application_guard::require_compatible_schema` now exposes the exact native
application startup seam: Gateway delegates its original required-object/ledger
window check (2–10); Knowledge API/worker require both ledgers, exactly one matching
baseline and epoch 5–17. It issues SELECTs using the supplied application
connection and neither changes roles nor installs migrations/grants. Gateway and
Knowledge owners are wiring this before activation. The source configuration
incompatibility under least-privilege roles is retained intentionally, as directed
by the parent; an inactive successor reader is not substituted.

Ordinary native binaries built by parent were then exercised offline by this
worker. Frozen render and grants drift checks passed. Successor invariant checks
and isolated external candidate20 materialization passed; offline role probe on
that candidate returned `database_actions:false`. Canonical17 probe refused as
expected because it requires externally composed20 source. No database variables
were read or database operations requested by these checks.

Current full-source policy generation refused the unreviewed table
`assistant_runtime_execution_owners`. The original source ownership resolver
raises the same unresolved-owner error, verified directly as an oracle. Its full
builder also requires Git metadata absent in this checkout. The native generator
does not invent a new ownership decision to force successful output. Logs are
under the verification workspace's `foundation-offline-tools` directory.

## Runtime-library and operational follow-on

After the user's code-only priority update, work switched away from database
validation. GenericREST now includes a contextual base-jsonpath-ng grammar fallback
for grouping, expression unions, where/wherenot existence predicates, named
this/parent, root rebinding, numeric field names, arbitrary-size index bounds and
Unicode string indexing. The source's unimplemented intersection remains an
evaluation error. 55 static goldens were captured from pinned jsonpath-ng1.8.0,
including its unusual precedence, parent-of-coerced-scalar behavior, false/null
existence matches and absent-parent failure semantics. Targeted Rust tests are
awaiting the parent's serialized run; no full parity claim before they pass.

Added `admin-create` (platform-auth) and `repair-usage-metrics` (migration-authority)
code only. Admin bootstrap preserves the environment inputs, bcrypt cost12 and
single INSERT with no upsert/password reset. The historical script imported a
removed `db` global; native code implements its intended explicit database/INSERT
operation. Usage repair preserves dry-run default, exact four source SQL strings,
output fields, and one atomic dedupe/rebuild transaction. Static SQL digest and
output tests were added. Neither command was run, and no account or usage data
was created/modified.

Response text decoding now embeds 66 single-byte source codec maps and 280 aliases,
including Python-specific undefined-byte replacement rather than WHATWG mappings.
UTF-16/32 text codecs preserve source BOM requirements, explicit-endian BOM text,
and truncated-surrogate replacement boundaries. Sixteen immutable httpx0.28.1
text-decoder goldens and map-integrity tests were added. These additions await the
parent's targeted test run. Multibyte legacy codecs still use encoding_rs; exotic
Python codecs and JSON values unrepresentable by serde_json remain unproven.

## Native inventory generator replacement

Implemented platform-tools `inventory-generate` for all six fact baselines and
native core consumption, persistence SQL and domain ownership inventories.
Provenance uses exact immutable Git commit/tree identities, clean tracked and
untracked checks, input equality, deterministic ordering and a post-scan fence.
Verification is default; writes require the explicit clean HEAD. Native facts
use v2 baseline envelopes and separate `native-rust-v2` destinations. Historical
baselines and historical domain-owner rationale remain unchanged.

Rust AST/Cargo source facts replace Python AST facts explicitly. SQL collection
includes literal macro arguments and external `include_str!` SQL, plus schema,
GRANT, SERIAL and function-body evidence. LOC reads Git blobs; skip producers
reuse the same Rust marker parser as the native hygiene consumer. Added an
in-memory synthetic fixture exercising all nine producers, deterministic output,
SERIAL/function attribution, external SQL inclusion and Rust/TypeScript ignores.
No baseline generation/write or new tests were executed by this worker; root is
serializing compilation and targeted unit tests. The registered live full OpenAPI
proof command is still pending, and the freeze generator states that honestly.

Parent's targeted foundation run completed successfully: expression/context test
(55 immutable source cases), 2 recursive-descent tests, and all 5 response/codec
tests passed. Latest inventory-generator additions still await targeted tests.

## Sealed release judge replacement

Implemented `platform-eval::judge_receipts` and the
`judge_real_agent_receipts` binary. Deterministic input preparation verifies the
external receipt digest and suite nonce, independent collector/golden HMACs,
source/plugin bytes, exact trial/check/assertion sets, current-attempt lifecycle,
terminal tenant scope, tool pairing/order and typed write/readback evidence.
Provider setup happens only after preflight and the code-pinned release manifest
checks. Two independently prompted DeepSeek calls retain fixed HTTPS origin/model
allowlists, no proxy/redirect following, bounded responses/retries, strict schema
and evidence-reference checks. Host aggregation uses integer hundredths and the
minimum dimension/trial/domain rule, preventing display rounding from passing a
subthreshold score. Reports redact secrets and use no-follow mode0600 writes.

Captured original-source offline fixtures for exact lane A/B prompts, lifecycle
summaries, and three independently challenged double-HMAC trial receipts. Added
native source-oracle, tampering, schema/evidence, settings/redaction and weighted
threshold tests. Rust tests await the parent's serialized run; no provider
requests were made. The old release manifest digest remains immutable. Its old
coding policy does not silently become the new Rust coding-v2 policy: mismatch
and the currently unbound native image pin continue to fail closed, requiring an
explicit reviewed release-artifact refresh rather than fabricated acceptance.

## Online sampling and outbox CI equivalents

Added production-used `eval_online` repository and `eval_outbox_dispatch`
adapter seams, plus `eval_online_outbox_contract` with eleven database-free
contracts. The native worker delegates to these algorithms; its SQL adapters
retain claim fencing, actor resolution, capacity admission and transactional
acknowledgements. Sampling now preserves source null/boolean/string coercion,
default-family filtering, internal-origin whitespace handling, queue caps and
100-row pagination. Tests cover 101 evaluators, active runs, both queue limits,
retry/exhaustion, lost claims, KB infrastructure versus semantic-review mapping,
and actual production SQL bind plans and explicit PostgreSQL parameter casts.
The SQL deduplication predicate is checked without claiming a live database proof.
No DB or provider requests were made. Rustfmt passed; compilation and target
execution are queued with the parent, not yet confirmed.

## Gateway boot and management authorization source ports

Added gateway test targets `agent_studio_authorization` and `agent_studio_boot`.
Authorization exercises actual Skills, MCP, Provider and Dashboard routers with
trusted fixed identities and backend fixtures that panic if a denied request
reaches storage or mutation. Six tests cover the source's eleven HTTP assertions.
This proves router admission, not live identity resolution or database behavior.
Boot covers executable linkage/configuration refusal and actual loopback HTTP
readiness paths/request IDs. The gateway owner corrected the production readiness
probe to emit the source `svc-` request ID. Full gateway graph construction has an
explicitly ignored real-binary subprocess fixture requiring an isolated,
provisioned PG/Redis config; it was not run. No test reads ambient `.env` files.
The two dashboard WebSocket source assertions exposed a missing native route;
the gateway owner is addressing it before those cases can be marked migrated.
Rustfmt passed. Both new targets await the parent's serialized compile/tests.

Dashboard WebSocket production route was subsequently implemented by the gateway
owner. Added both missing source permission cases to `agent_studio_authorization`,
including actual loopback 101 upgrade/snapshot/ping-pong and pre-upgrade HTTP403
denial, plus the production rejection helper's requested 4003/reason assertion.
This now maps all thirteen source authorization cases into eight native tests.
These additions remain unrun pending the parent's serialized test invocation.

## Retirement map and MQ production transport

Created `reports/rust-migration-foundation-source-map.json` with 163 explicit
source-file entries and verified native counterpart paths. Statuses distinguish
concrete counterparts, conditional HTTP/codec boundaries, Python-only extension
contracts, and obsolete import/framework mechanisms. No files were deleted.
Repository-wide `graph_module` search found only source adapter/connector
implementations, no shipped configured graph; arbitrary external Python graph
modules need a native adapter and are not claimed executable after cutover.

Found and implemented a genuine missing transport: `message_queue_transport.rs`
now supplies real lazy Redis Streams publish/consume/ack/fail, retry-first reads,
consumer groups, original message metadata and dead-letter routing. Source quirks
remain: only the first returned message is processed, failure payloads are
process-local, and XADD before XACK is deliberately nontransactional. GenericREST
and inherited Dify now select MQ through their actual production adapter factory.
GenericREST passes only `json` to connector requests, preserving source behavior
rather than promoting body fields into ack/fail keyword arguments.
Four database-free backend-contract tests were added; no Redis call was made.
Rustfmt passed; Cargo compile/tests remain parent-owned and pending.

Parent confirmed targeted19: boot 2 passed/1 ignored; online-outbox 11 passed.
The first authorization execution exposed a test-fixture merge issue: standalone
routers each retained a fallback. Corrected the fixture to use `reset_fallback`,
matching the already-correct production mount pattern. Its 2 WebSocket cases had
passed; all 8 await the parent's rerun.

MQ also preserves deferred Redis URL/client construction and close/reopen client
ownership: an injected client is detached without closing; subsequent operations
create an owned client as the source does. Added a fifth no-network contract.

Added authority `kb_migration_contract`, five explicitly ignored PG tests covering
full-chain layouts/idempotent ledger/ownership/FKs, fresh97/retention112, actual
CLI/library lock serialization, TERM/KILL rollback, and dump/restore101 old-writer
compatibility. They require explicit disposable database creation permission via
test env flags, never load `.env`, and are unrun. Specialized embedding/artifact
source cases are not claimed covered by these five; the wider old gate must retain
that explicit coverage gap until their native assertions are supplied.

### Native request-governance upgrade

Implemented shared validated configuration and Axum/Tower middleware with immediate
process-level concurrency admission, HTTP 503 overload, header-only HTTP 504 timeout,
body-idle errors, SSE-safe unlimited total duration, body-lifetime permits and existing
AdmissionCancellation extension propagation. Gateway production host constructors and
typed settings are wired; Knowledge owner wires its own production composition.
Six offline tests cover overload/probes, header cancellation, streaming lifetime,
idle expiration, caller drop, and invalid configuration. Rustfmt ran; Cargo execution
is queued with the root coordinator, so these new tests are not yet reported passing.

Fourteen embedded incremental multibyte codec tables now preserve source replacement
semantics for Japanese, Chinese and Korean labels instead of WHATWG approximations;
1,190 static byte-input oracle cases and table integrity tests are queued. This does
not claim all Python codec names (such as stateful ISO-2022) have exact native mappings.

Judge release loading now explicitly pins the native-v2 manifest and current native
coding validator/scenario identities, retaining the original scoring thresholds.
The historical v1 manifest is unchanged. Artifact consistency is a unit-test target;
no coding sandbox/provider acceptance was run.

The production ComfyUI adapter now reuses the exported native JSON WebSocket
connector (caller-owned duplex socket, control frames, Unicode binary JSON,
explicit close); two loopback contracts are ready. Native HTTP transport now
exposes bounded one-time HEAD pool warm-up and explicit close, used by the real
adapter shutdown hook. Registry shutdown closes cached adapters concurrently after
worker/request drain and before database/Redis teardown. Bulkheads forward the
hook, and failed/cancelled cleanup remains retryable. The MQ owner implemented its
atomic stream-drain override on this actual lifecycle path. Registry retry and HTTP
pool lifecycle tests are added but not yet run by this lane.

Dashboard upgrade now retains its own active-session budget through the entire
socket task (default 256). Snapshot/send operations are bounded to 10 seconds;
60-second inbound idle detection uses protocol pings so passive browser dashboards
remain live. Auth and 4001/4003 classification precede admission unchanged; saturation
is HTTP 503 before upgrade. Two added loopback tests assert post-101 capacity,
disconnect release/re-admission, and idle-peer cleanup. These are code-only pending
the root's next targeted batch. Production edits frozen after this bounded patch.

### Verified targeted results after native upgrade

Root-serialized execution, read from completed logs:
- `foundation-native37.log`: adapter_shutdown 1 passed; HTTP transport lifecycle
  1 passed; multibyte oracle 1 passed covering all 1,190 static cases.
- `governance-ws38.log`: request governance 8 passed; JSON WebSocket transport
  2 passed. These are actual in-process/loopback contracts, not live provider proof.
- The MQ target in batch37 failed one drain case (13 passed, 1 failed); the MQ owner
  owns the correction and subsequent verification. This is not counted as a pass.
- Dashboard budget/idle additions and native-v2 release-pin unit remain pending
  their dedicated root-run test targets at this report update.

Production and test sources in this lane were frozen at 2026-10-15 13:06:58 UTC.
The runtime-worker lane subsequently took narrow WebSocket TLS-provider ownership
for compatibility with the latest dependency set; that change is independently
tracked by the coordinator. Retirement-map counts now reflect 167 source entries:
134 concrete counterparts, 3 conditional shared HTTP/codec entries, 4 explicit
external-native-extension boundaries and 26 framework-only retirements.

Root batch `foundation-native43.log` subsequently passed all six selected targets:
adapter_shutdown 1; multibyte oracle 1 (1,190 cases); HTTP lifecycle 1; upgraded
message_queue_transport 15; request_governance 8; websocket_transport 2. The earlier
MQ drain failure is therefore resolved in its owner's later code. Root
`workspace-check41.log` completed all-target checking successfully with the latest
compatibility updates. `gateway-native44.log` was terminated by the root's disk
usage guard before a test result; this does not verify the new dashboard budget
cases. Native-v2 judge artifact unit remains unverified here until its target runs.

Final documentation checkpoint (2026-10-15 13:58 UTC): root batch
`gateway-native45.log` passed authorization 10, served native OpenAPI 1 and label
regression 1. This verifies the dashboard active-session/idle additions that were
pending above. Current consolidated scope, results and remaining retirement/runtime
boundaries are in `docs/migration/full-rust-status.md`; older checkpoints remain
historical evidence. No hook installation, source-code edits or builds were performed
during this final documentation closure.
