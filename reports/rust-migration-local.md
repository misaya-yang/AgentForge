# Native local node and SDK migration slice

## Implemented source

New `ai-platform-local-node` crate (Unix file operations; other modules portable):
- Explicit signed action/approval models, canonical payload/digest, expiry and scope validation. Verifier authority is injected independently, never accepted from JSON.
- Durable one-use local approvals backed by SQLite unique nonces, verification before consumption.
- SQLite action ledger: atomic idempotency reservation, envelope binding, state transitions, terminal immutability, redaction before storage, append-only hash chain and seal verification, disconnect marking pre-dispatch interrupted and dispatched/running unknown.
- SQLite ordered receipt outbox: bounded content retention only by explicit request, contiguous ACK validation, erasure of acknowledged content and WAL checkpoint.
- HTTPS-only outbound transport: no redirect/downgrade, device credential header only, bounded JSON responses, exact exchange response schema and protocol version.
- Durable command/nonce replay guard and signed claim dispatcher to terminal outbox receipt. No implicit re-execution on duplicate claim.
- Explicit owner/capability/expiry/revocation-bound directory grants; descriptor-relative no-follow Unix traversal; root identity pinning; NFC, traversal, sensitive-file, symlink and hardlink rejection; bounded reads.
- Approved file writes with expected content hash, atomic replacement, fsync and private durable rollback data and approval-bound restoration. Actual local file I/O, not a Python wrapper.
- ProcessRunner implements allowlisted argv, clean environment, descriptor-pinned cwd, local approval, bounded stdout/stderr, timeout, process-group cancellation and durable receipt. Network deny remains unavailable without a sandbox, matching Python's explicit fail-closed behavior.
- ComputerController implements exclusive expiring leases, session/app/origin scope, observation binding, per-action approvals and unknown-after-input failure handling. Native macOS adapter invokes the existing Swift AX/CGEvent helper directly with trusted executable checks, bounded output, explicit permission probes and sensitive UI takeover checks.
- Secure credential-store interface, conditional macOS Keychain adapter, OS-random device identity, signed pairing redemption, persisted credentials, and bounded heartbeat/exchange runner. Doctor CLI matches retained diagnostic flags and deliberately performs no network/credential operations.
- Signed file list/read/hash/search/watch dispatch with read/search/watch budgets, polling rename/change events and grant revalidation. Multi-file analysis validates all selected paths and budgets before reading; grant store enforces unique identities and revocation.
- Native SDK SSE decoder preserving explicit event fields and Python event/data projection, with tests authored against shared SDK fixtures. Existing V2 decoder remains unchanged.
- Native Assistant SDK methods for chat/streaming, image generation and unknown admission outcomes, task polling/cancellation, skills, tools, sessions, artifacts/download, thread turns and approvals. Desktop maps uncertain-submission errors without silently retrying.
- MCP stdio and HTTPS/literal-loopback HTTP clients implement initialize, tool discovery/calls, JSON-RPC identity checks, bounded framing, session headers and namespaced manager lifecycle. Explicit HTTP transport adds SSE support; no redirect credential forwarding.

## Verification

Tests authored for a frozen Python-generated signed-envelope golden fixture (no Python dependency at test time), allowlisted subprocess execution and output redaction, CLI secret-safe errors, API-key owner headers, uncertain image admission, actual approved writes/rollback, canonical numeric payloads, durable action replay, unknown-terminal preservation, scope conflicts, one-use approval persistence, ACK range/content erasure, nonce replay, symlink/hardlink rejection, revoked grants, sensitive paths and shared SDK SSE envelopes.

No Cargo command, Docker action, live endpoint call, device operation, or secret read was performed by this worker. Parent reports an earlier local-node snapshot passed 8 tests; later additions require the current gate. Parent owns serialized build/test evidence. Source existence and test authoring are not a passing test claim.

## Explicit remaining work / no full-cutover claim

- Production composition retains the original requirement for independently injected platform signature verifier and native pairing signer. No algorithm/config redesign, permissive verifier or plaintext fallback is shipped. Standalone CLI preserves Python doctor-only semantics; the native runner is composed in-process just as the original runner was.
- Native rollback restores files or removes newly created targets and now uses Python-compatible base64 rollback records. Existing production backup/database interoperability still needs a migration acceptance test.
- Multi-file analysis, explicit screen-capture artifacts and macOS approval prompt wrapper are implemented. Full controller/backend fault-matrix tests remain; native helper/Keychain behavior has not been tested on macOS.
- Signed cancel/emergency-stop routing and mandatory lifecycle callbacks are implemented; the controlled exchange loop persists disconnect unknown receipts. Public compositions must implement ControlledDispatcher with actual process/computer stop behavior.
- Broader golden-vector canonical numeric serialization (Python float exponent/fixed formatting normalization and representative vectors are implemented), ledger compatibility against existing Python database files and crash fault injection require further evidence. Do not enable live cutover solely from these unit tests.
- SDK timestamp normalization, typed request/response models and canonical AssistantClient module facade are implemented. GET/HEAD retry/backoff, retry-after and no-mutation-replay are implemented. Complete recovery edge-case parity and MCP live integration evidence remain. HTTP MCP preserves its configured endpoint path rather than Python's root-path behavior.
- Approval/signature traits deliberately require a trusted composition root; no permissive production verifier was added.

Python/browser TypeScript remains intact. This slice adds native implementations but does not justify deleting the retained implementations or claiming full runtime parity.

## Verified native gate update
Parent's serialized `config-client-local-tests.log` now records the current client tests (35 + 11 + 3), config tests (6), and local-node tests (5 + 2 + 8) passing. This supersedes the earlier 8-test-only snapshot. macOS conditional adapters have not been executed on macOS.

## Evaluation tooling extension (in progress)
`ai-platform-eval` replaces the independent evaluation tools without importing or launching Python. Implemented RAG validation/gating, duplicate-aware retrieval metrics, Python-compatible MT19937 bootstrap sampling, pinned recorded-judge policy/provenance checks, seed JSONL generation/checking/import-payload rendering, and live retrieval observation recording/manifest verification. Golden behavior validation, stateful scoring and count-backed gate have been ported; CLI/provenance/candidate adapter and runtime-regression orchestration are being completed. Root reported the initial RAG/record/seed snapshot compiled; no evaluation test pass is claimed until root runs it. Existing JSONL fixtures remain immutable acceptance inputs. Seed families are separate static source data, not read back from the expected output.

## Native evaluation expansion (2026-10-15 11:41 UTC)

Implemented source-equivalent native CLI paths for golden/RAG validation and gates,
RAG observation recording, KB golden seeding, runtime regression aggregation,
Agent Studio aggregation, failure-recovery evaluation, finance evaluation, TTFT
benchmarking, real-domain scenario collection/validation/merge, and coding host-test
receipt verification/execution. Producer adapter and parity support were supplied
by the runtime lane; resource profiling, Gateway adapter and CRA evaluation by the
root lane. The independent release judge is being ported in the authority lane.

Scenario evidence preserves strict duplicate-key rejection, immutable source and
plugin hashes, independent collector/golden HMACs, fresh nonce/challenge/session/
attempt/stream identities, private raw SSE, reconstruction equality, deterministic
assertions and execution checks, and host recomputation before merge. Coding adds
an independent third key and constrained network-disabled container execution.
No live provider call, sandbox execution, credential read or device operation was
performed in this lane while authoring these paths.

Root reported `eval-release-tests4` passed 52 library tests, 4 golden integration
and 2 RAG integration tests, plus 8 release tests. That snapshot predates the
latest scenario and coding additions, which require the next compiler/test gate.

The new Rust `coding_parallel_v2` fixture retains floor-allocation loss and
nested-object-order-sensitive fingerprint defects. Its five acceptance tests are
intentionally not a green baseline: four should fail until repaired. New source,
contract, delegation and receipt hashes prevent reusing v1 evidence. A verified
native sandbox image and locked/vendored dependencies remain unprovisioned; the
image is explicitly null and validation rejects it. This is an honest release
blocker, not an implicit waiver.

Remaining manifest cutover work includes source-specific Agent Studio test
matrices not yet ported (agent API/RBAC/migrations, connector, MCP, skill,
publication and observability contracts). Replacing them with unrelated broad
Cargo success would weaken gates and has not been done. Native runtime integration
targets remain explicit, with zero-test runs rejected by the aggregator.

### Studio native gate cutover (2026-10-15 12:08 UTC, source authored)

The finite AS-00 through AS-08 manifest keeps all phase IDs and required gate IDs. Matching phase runbook commands now use explicit Rust test targets and `--include-ignored`; lint commands use Clippy. The runner rejects native ignored tests as skipped and rejects Cargo gates without a positive passed-test summary. The `studio_manifest` integration suite checks manifest/runbook exact equality, critical group retention, and skip handling. No aggregate gate was executed here.

Retained behavioral case mappings:
- Internal capability proof scope/body/path/expiry, independent token, bounded read projection: `agent_studio_capability_proof`.
- Knowledge capability HTTP authentication, current actor roles, dataset ACL non-disclosure, authoritative retrieval forwarding/projection, non-text rejection, and signed dataset/body/scope: `agent_studio_knowledge_capability` (five source cases). The narrow injected backend delegates unchanged production actor/ACL/retrieval adapters.
- Runtime signed envelope identity, replay, canonical hashes, TTL, draft/version pins: `agent_studio_envelope`.
- Connector catalog/grant policies, tenant query bindings, disabled/revoked/public rejection: `agent_studio_connector`.
- MCP discovery reuse with reauthorization, protocol mismatch, closed schemas, secret-reference/error redaction, OAuth metadata, execution row lifecycle and argument binding: `agent_studio_mcp`. End-to-end MCP upstream invocation and full management CRUD HTTP remain incomplete.
- Skill entrypoint/code policy and tenant registry isolation: `agent_studio_skill_entrypoint`, `agent_studio_skill_isolation`. Skill body coercion/omission/nulls, builtin read/test/read-only HTTP: `agent_studio_skills_api`. Full tenant CRUD/version storage HTTP coverage remains incomplete.
- Embed route framing, Helm/Nginx proxy headers, security middleware path exemption, ownership-safe smoke script, credential-free assets: `agent_studio_embed_headers` (five source cases).
- Studio CRUD/database/publication/RBAC/runtime/observability/eval targets are shared with the runtime worker. Their existence does not establish complete replacement of the large source HTTP suites; stateful tests require explicitly provisioned isolated database/Redis and are excluded from offline success claims.
- Gateway boot/management authorization is assigned to the authentication owner as `agent_studio_boot` and `agent_studio_authorization`.

Language-specific retirement: Python module import paths and Pydantic class/mock identities are not Rust acceptance requirements. Their externally observable schema/error/security behavior remains required. Native compilation/linking replaces Python import availability checks; method-call expectations are retained only where they express authorization-before-side-effect or exact request forwarding. Historical receipts cannot establish passage of the changed native manifest hash. Latest additions above are authored/formatted, pending the parent's serialized compile/test result.

### Coding fixture offline provisioning (2026-10-15 12:17 UTC)

The prior unbound-image blocker is resolved in code/metadata: verified official Rust 1.99.0 slim-bookworm linux/amd64 manifest/config identities are separately bound, and the parent's standalone Cargo lock/vendor output is retained. A read-only Node checksum audit verified 25 packages/1179 files, preserving 49 license/notice files and finding zero Python files. Native `verify_vendor` rejects changed/missing/additional vendor bytes and runs before provider collection, along with a preloaded-image identity check. The fixture scenario now binds 15 source artifacts including lock/config/vendor inventory/image metadata/validator contract. No layers were fetched, sandbox executed, or candidate benchmark run. The sandbox's memory/time/tmpfs limits remain unchanged and require execution validation with the compiled native fixture.

### Native acceptance and examples (2026-10-15 12:35 UTC)

Both original local acceptance programs now have actual native binary implementations. The local runner exercises fresh disk state, bounded watcher events, approved atomic writes/replay/rollback/stale preservation, clean argv subprocess streaming/cancellation, disconnect and SQLite tamper evidence. The macOS runner uses the retained Swift helper, exact current observation screenshot, Calculator Accessibility readback and Keychain-backed explicit approval. New optional process-output callback preserves the existing run method. No acceptance program was run here. The SDK demo and publication/attachment/SSE example are translated native Cargo examples, also not run. Source retirement mapping and the unresolved external Python code-interpreter language boundary are recorded in `rust-migration-local-retirement-map.md`; new code awaits parent compilation.

### Native stream lifecycle refinement (2026-10-15 12:51 UTC)

The existing SSE API remains unchanged. SDK streams now share bounded header/body waits, SSE media-type validation and a cancellable receiver-aware flush path, including an unterminated final frame when the channel is full. A receiver dropped before invocation suppresses admission. V2 read-only event watching observes consumer drop during response headers, idle body and retry backoff, and resumes only its existing server-issued `after_sequence` cursor; duplicate sequence numbers are suppressed. POST streams are never replayed. The nine authored `stream_reliability` loopback/pure tests cover these phases, fragmented UTF-8 and size caps, with no provider or device operation. Parent compile/test results remain pending for this refinement.

Local-node transport review confirms both native sides currently implement HTTPS JSON heartbeat/exchange, with WSS accepted only as an unavailable injectable-adapter policy option. There is no established WebSocket device wire exchange to preserve. Adding a guessed WebSocket protocol would change the server contract; no such fallback or false readiness claim was introduced.

Parent reported all nine stream lifecycle tests passed in gate 32. The follow-on
configurability refinement exposes validated `StreamLimits` through both
`Client::with_stream_limits` and `AssistantClient::new_with_stream_limits`, keeping
30s/90s defaults and allowing positive per-phase durations up to 24h for explicit
long-reasoning deployments. Header/idle overrides apply to SDK streams and V2
GET observation; cancellation and mutation no-retry remain unchanged. Three
additional tests cover limits and actual header/idle timeout behavior; the
12-test target awaits the parent's next serialized test gate.

### Final dependency and delivery review (2026-10-15 14:05 UTC)

The final workspace uses reqwest 0.13.5. Its library-default protocol-NACK retry
policy is now explicitly disabled on all six client/MCP/stream/example builders;
application GET retry/resume remains explicit and mutation admission is not
replayed by the HTTP library. Parent `client53` passed all 13 stream reliability
tests, including typed timeout overrides and the six-builder policy contract.
The parent also corrected local-node transport and eval/release builders.

The macOS acceptance binary now embeds its retained Swift source and materializes
it exclusively with mode0600 in the existing private temporary workspace before
compilation. Installed binaries no longer depend on the original compile-time
checkout path. This is relocation/source completeness, not live macOS evidence.
Remaining first-party test mappings and their non-equivalent live/large-HTTP
coverage obligations were relayed to the source-retirement owner. This lane's
port code is frozen; no provider/device/acceptance run was performed here.
