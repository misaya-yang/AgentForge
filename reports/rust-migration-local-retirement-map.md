# SDK, local node, desktop and evaluation source retirement

Updated 2026-10-15. This maps source responsibilities to authored native code. It is not a live acceptance receipt. Root owns removal of historical sources and deployment references after the serialized native compiler gate. JSON/JSONL fixtures, SQL, Swift helper, frontend TypeScript, licenses and benchmark provenance remain inputs.

## Runtime and SDK replacements

| Historical source | Native replacement under `rust/platform-native/crates` | Preserved boundary |
|---|---|---|
| `apps/local-node/src/local_node/{models,errors,grants,workspace,files}.py` | `platform-local-node/src/{envelope,lib,workspace,files}.rs` | Identity/digests, grant/root/escape policy, exact read/search/hash, atomic writes and rollback |
| `.../{ledger,approvals,file_dispatch,outbox,watcher}.py` | Same-named Rust modules | Durable idempotency, one-use approval, event chain, bounded dispatch/outbox and metadata-only watch |
| `.../{identity,credentials,processes,computer,macos_native}.py` | `identity.rs`, `credentials.rs`, `processes.rs`, `computer.rs`, `macos.rs` | Platform credential adapters, allowlisted argv/clean environment/cancellation, scoped observation and native helper input |
| `.../{transport,service,doctor,cli}.py` | `transport.rs`, `service.rs`, `runner.rs`, `bin/local-node.rs` | Outbound transport/receipt and disconnect semantics; explicit verifier injection and readiness refusal; daemon/CLI |
| `apps/local-node/scripts/local_live_acceptance.py` | `platform-local-node/src/acceptance.rs`, bin `local_live_acceptance` | Same six evidence groups, real temporary files/watcher/process/SQLite assertions and explicit test-only platform signer |
| `apps/local-node/scripts/macos_computer_live_acceptance.py` | `platform-local-node/src/macos_acceptance.rs`, bin `macos_computer_live_acceptance --report-dir PATH` | Native Calculator screenshot/action/readback, Keychain-backed exact-intent approval, lease and emergency-stop assertions |
| `sdk/python/ai_assistant/{client,auth,chat,sessions,knowledge,images,artifacts,tools,skills,runs,threads}.py` | `platform-client/src/{sdk,sdk_models,assistant_sdk,assistant_runs,sdk_threads,request,lib}.rs` | Typed public facade and HTTP path/payload/auth/retry/recovery contracts |
| SDK `transport/http.py`, `streaming/sse_parser.py`, `models/*.py`, `exceptions.py` | `request.rs`, `sdk_stream.rs`, `sse.rs`, `sdk_models.rs`, native `Error` | Incremental UTF-8/SSE framing, inner envelopes, cancel/close, transport errors and data types |
| SDK `mcp/{client,manager}.py` | `platform-client/src/mcp.rs` | Discovery/call lifecycle and bounded transport; no Python subprocess wrapper |
| `sdk/python/demo.py`, `examples/agent_runtime.py` | `platform-client/examples/{demo,agent_runtime}.rs` | Same environment variables, chat/knowledge/tools demo and scoped publication session/attachment/stream example |
| Desktop/CLI interfaces and packaging | `platform-desktop/src/migration_desktop`, `platform-client/src/bin/platformctl.rs`; shared `platform-tools/src/packaging.rs`, portable `platform-client-packaging` | Native interfaces and package identity/help/archive checks; packaging owner supplies both commands |

Acceptance programs are explicit operator commands, not automatically executed tests. The macOS runner can compile the retained Swift helper, request approval and write its receipt artifact only when explicitly invoked. Neither runner was executed during this port. Native report host metadata replaces the Python interpreter version with implementation identity; changed harness identity cannot reuse historical receipts.

## Evaluation entrypoints

All following script stems have corresponding native binaries in `platform-eval/src/bin`:

- `eval_rag`, `regen_rag_observations`, `seed_kb_golden_set`: `rag`, `record`, `seed`, `metrics`, `strict_json` modules preserve JSONL validation/record/gate and seed contracts.
- `eval_golden`: `golden`, `golden_validation`, `golden_gate`, `golden_stateful`, `golden_cli`, `judges`, `producer` preserve pure/stateful adapter and thresholds.
- `assistant_runtime_regression`, `agent_studio_regression`: `runtime_regression`, `studio`; finite group IDs, manifests and positive executed-test requirements remain. Current Studio runbooks and manifest have matching native target commands.
- `assistant_capability_live`, `assistant_ttft_benchmark`: `assistant_capability`, `ttft`, `ttft_stats`, `bootstrap`, `live`; same explicit provider/execution gates, measured events and private evidence writing.
- `eval_general_agent_failure_recovery`, `validate_real_finance_eval`: `failure_recovery`, `finance`.
- `real_agent_scenario_runner`: `scenario`, `scenario_contract`, `scenario_events`, `scenario_runner`, `scenario_evidence`, `seals`; independently signed evidence and host assertions remain mandatory.
- `native_agent_parity_benchmark`, `ppr00_resource_profile`, `collect_cra_compliance_research`, `judge_real_agent_receipts`: shared-lane `parity_benchmark`, `parity_support`, `resource_profile`, `resource_stats`, `cra`, `judge_receipts`.
- `verify_coding_parallel_fixture`: `coding_receipt`, `coding_sandbox`; Rust v2 fixture replaces Python task/tests, has new hashes, locked/checksummed vendored dependencies and separately pinned official OCI manifest/config IDs. Original acceptance bugs remain intentional. No sandbox execution is claimed.

Knowledge-specific bakeoff/import/backfill/sparse/shadow tools belong to the Knowledge lane. General harness/source-lock/evidence/topology/resource-lock tooling belongs to the SDK-service lane. Neither is silently represented by the eval binaries above.

## Test retention and language-specific retirement

- Local node source file/ledger/process/outbox/transport/computer/identity tests map to in-module native tests plus `platform-local-node/tests/{security,cli}.rs`. The new acceptance runners exercise their original live scenarios only when explicitly invoked. Linux checks do not establish macOS Keychain, Accessibility, screen permissions or native GUI behavior.
- SDK source tests map to native module tests and `platform-client/tests/sdk_transport.rs`; shared `sdk/fixtures` SSE JSON remains. Native request/recovery/cancellation/unsafe-redirect tests retain security assertions. New examples are compiler targets, not automatic live provider calls.
- Eval pure and fixture tests live in `platform-eval/tests` and module tests. Live/DB/provider benchmark evidence remains unavailable until its explicit native command runs against authorized infrastructure.
- Studio targets retain the behavioral groups described in `rust-migration-local.md` and the runtime lane retirement map. Auth/boot owner now supplies `agent_studio_authorization` (all 13 source permission/WebSocket cases in 8 tests) and `agent_studio_boot`; full graph startup remains an explicit ignored infrastructure test. Remaining large HTTP variants are coverage gaps, not waived requirements.
- Retire `__init__.py`, Python package import/path availability, interpreter-version assertions, pytest registration/conftest plumbing, Python/Pydantic object identity and private monkeypatch call-shape checks where they have no independent externally observable effect. Cargo compilation/linking and native test setup replace those language mechanisms. Schema/error content, authorization-before-side-effect, exact forwarding, replay protection, data identity and gate thresholds are not retired.

## Explicit zero-Python execution-language conflict

`rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/python_code_execution.rs` is already Rust implementation. Its `execute_python_code` capability intentionally executes supplied Python language code in a constrained external CPython runtime. `docker/code-interpreter/Dockerfile` and its requirements supply NumPy, pandas, SciPy, matplotlib, seaborn, openpyxl, xlrd, dateutil, plotly, python-docx and tabulate. This external execution-language dependency is incompatible with a literal prohibition on any Python runtime/source in the shipped interpreter image.

An AST parser is not an interpreter, and a limited RustPython evaluator would not preserve these native-extension/package assumptions. At the root's explicit direction this capability remains intact rather than being replaced by an incomplete VM, silently disabled or mislabeled migrated. A product decision on supported execution languages/packages is required before claiming literal zero-Python runtime for this optional image. No interpreter image pull/build/run or new VM dependency was performed.

## Evidence status

Earlier serialized parent tests passed the SDK/local/config and eval snapshots recorded in `rust-migration-local.md`. The latest acceptance modules, output callback, exact screenshot accessor and two examples are authored/formatted and await the next serialized compiler/test gate. This document does not convert authored code into a passing operational receipt.
