# SDK worker and operational-tools retirement scope

This is a source-move review map, not permission to delete data and not evidence
of a live deployment. Move the listed replaced implementations to the external,
recoverable reference snapshot only after the root verification gate. Native
code is written and prior snapshots compiled; newer changes await the root's
serialized test results. No paid provider, collector, Docker, or PostgreSQL
acceptance was performed by this lane.

## Replaced implementations owned by this lane

All paths below are exact repository paths. `tools` means
`rust/platform-native/crates/platform-tools/src/` and `platform-tools` is the
native command. The command implementations perform real validation/work, not
Python delegation.

| Python implementation | Native implementation / command |
|---|---|
| scripts/deploy/topology_modes.py | tools/topology.rs; platform-tools topology |
| scripts/harness/check_harness.py | tools/harness.rs; harness-check |
| scripts/harness/affected_gates.py | tools/selection.rs; affected-gates |
| scripts/harness/ci_gate_enforcement.py | tools/ci.rs; ci-gate-enforcement |
| scripts/harness/import_boundary_gate.py | tools/architecture.rs; architecture-boundary, native Cargo boundaries and exact reviewed exception |
| scripts/harness/hygiene_check.py | tools/hygiene.rs; hygiene-check, actual Rust syn test AST |
| scripts/harness/loc_no_growth_gate.py | tools/loc.rs; loc-no-growth; native Rust/TypeScript baselines versioned separately |
| scripts/harness/single_instance_guard.py | tools/guard.rs; single-instance-guard and topology-inventory |
| scripts/inventory/service_topology.py | tools/guard.rs + topology.rs; live Compose/Helm/native route facts, source format validation |
| scripts/harness/rust_changed_crate_gate.py | tools/rust_gate.rs; rust-changed-crate-gate, separate native1.99/overlay1.95 identities |
| scripts/harness/rust_gate_identity.py | tools/rust_gate.rs + source.rs/source_refresh.rs; pinned source/toolchain identity and preimages |
| scripts/harness/agent_runtime_source_candidate.py | tools/source.rs; source-candidate verify/assemble |
| scripts/harness/agent_runtime_supply_chain.py | tools/source.rs + source_refresh.rs; source-lock validate/generate/generate-composed/refresh-source-lock/refresh-overlay/record-local-image/verify-local-images/verify-overlay |
| scripts/harness/native_candidate_plan.py | tools/native_gates.rs; native-candidate-plan |
| scripts/harness/agent_runtime_single_kernel_gate.py | tools/native_gates.rs; agent-runtime-single-kernel-gate, actual Rust dispatch AST |
| scripts/harness/agent_runtime_readonly_gate.py | tools/native_gates.rs; agent-runtime-readonly-gate invokes Gateway tests/readonly_contributor_gate.rs |
| scripts/harness/agent_runtime_write_gate.py | tools/native_gates.rs; agent-runtime-write-gate builds pinned composed runtime and invokes actual native library contract tests |
| scripts/harness/agent_runtime_text_gate.py | platform-gateway/src/bin/agent-runtime-text-gate.rs; actual isolated runtime/model/accounting/text/restart driver, unrun live |
| scripts/harness/gateway_performance_baseline.py | tools/performance.rs; reusable bounded actual HTTP/process measurement API, gateway-performance-baseline retains original plan-only CLI |
| scripts/harness/package_desktop.py | tools/packaging.rs shared with platform-client-packaging; package-desktop, real binary/identity/archive checks |
| scripts/harness/package_platformctl.py | same shared packaging.rs; package-platformctl |
| scripts/evidence/artifacts.py | tools/evidence.rs; evidence validate/status/cleanup, dry-run default and explicit hash-bound quarantine authorization |
| scripts/evidence/known_candidates.py | tools/candidates.rs; known-candidates |
| scripts/rust/resource_lock.py | tools/resource_lock.rs; resource-lock, real global/resource file locks, receipts, child/process-group cleanup |

Associated empty namespace initializers in scripts/deploy and scripts/rust can
move with these packages. Do not treat an unknown scripts subtree as covered by
this list.

### Source declarations and tests whose mechanics changed

The architecture, hygiene, source-identity, topology and single-kernel gates now
inspect Cargo/Rust declarations rather than Python import/FastAPI/private AST
objects. Tests asserting Python AST/private helper structures, interpreter
invocation, module import paths or exact Python package layouts are obsolete
implementation tests. Their native replacements are module unit tests in the
files above, plus genuine Rust AST/canonical digest/negative-rule fixtures.
This is not a claim that every assertion from each old test file was replayed.

Retirement-review candidates tied to those replaced implementations:

- tests/harness/test_agent_runtime_composed_receipt.py
- tests/harness/test_agent_runtime_readonly_gate.py
- tests/harness/test_agent_runtime_source_candidate.py
- tests/harness/test_agent_runtime_supply_chain.py
- tests/harness/test_agent_runtime_text_gate.py
- tests/harness/test_agent_runtime_write_gate.py
- tests/harness/test_check_harness_gate_schema.py
- tests/harness/test_ci_gate_enforcement.py
- tests/harness/test_evidence_artifacts.py
- tests/harness/test_gateway_performance_baseline.py
- tests/harness/test_hygiene_check.py
- tests/harness/test_known_quality_candidates.py
- tests/harness/test_loc_no_growth_gate.py
- tests/harness/test_rust_changed_crate_gate.py
- tests/harness/test_single_instance_guard.py
- tests/scripts/test_rust_resource_lock.py
- tests/scripts/test_topology_modes.py

The following four suites were explicitly ported together into
`platform-tools/tests/deployment_script_contracts.rs` and `tests/deployment/`:

- tests/scripts/test_runtime_script_topology_contract.py
- tests/scripts/test_validate_env_quickstart.py
- tests/scripts/test_web_runtime_config_entrypoint.py
- tests/scripts/test_seed_demo_data.py

Their 75 original test functions/scenario groups are consolidated into 43 Rust
tests, using real shell scripts with isolated fake Docker/curl/authority commands.
Changed assertions are documented native command/image replacements, not skipped
health/identity checks. Root must run the new target before claiming it passed.

`tests/integration/test_gateway_openapi_contract.py` has a direct native port in
tools/live_openapi.rs (`live-openapi-contract`): actual HTTP GET, exact path
superset and retained required-body fields including schema refs, explicit
SKIPPED on unavailable stack. This is not a complete OpenAPI schema-equivalence
claim. No live request was executed.

Other tests/scripts and tests/harness files are NOT blanket-owned by this list.
Backup fault injection, release guards, provider journeys and specialized DB
assertions need their owning lane's coverage/retirement judgment.

## SDK service implementation

All implementation modules under `apps/sdk-agent-worker/sdk_agent_worker/` map
to `rust/platform-native/crates/platform-sdk-agent-worker/src/` (HTTP admission,
API, SSE, lifecycle, durable state, supervisor, bridge) and
`rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/src/` (provider
loops, tool calls, approvals and child protocol). Exact source module list:

- __init__.py
- capabilities.py
- child.py
- contract.py
- engines.py
- knowledge.py
- launch_frame.py
- lifecycle.py
- runtime.py
- server.py
- supervision.py
- tools.py
- usage.py

`apps/sdk-agent-worker/tests/test_official_results.py` asserts official Python SDK
object/result types. Those SDK-internal Python objects have no literal Rust
counterpart; native provider wire result handling and usage/history tests replace
that mechanism. Preserve the original in the reference snapshot. Native service
and composed service-to-engine loopback tests are real and were previously run
by root, but they do not certify paid-provider equivalence.

The native implementation is explicitly `native-provider-protocol`, not a claim
to be an official Rust OpenAI Agents/Claude SDK. Exact pinned OpenAI SQLiteSession
history import is implemented read-only; opaque legacy Claude SDK state is
rejected instead of silently dropping history. See rust-migration-sdk-service.md
and engine COMPATIBILITY.md for the unavoidable language boundary.

## Coordinated replacements owned by other lanes

These are confirmed pointers, not additional ownership claims:

- scripts/harness/agent_runtime_eval_contract.py → platform-eval runtime_contract module and agent-runtime-eval-contract binary (runtime worker).
- scripts/harness/runtime_durable_recovery_gate.py, runtime_recovery_observe.py, runtime_recovery_reference.py → migration-authority same-name underscore binaries (runtime worker).
- scripts/harness/gateway_kb_boundary_gate.py, kb_release_evidence_gate.py → platform-openapi gateway-kb-boundary / kb-release-evidence (Gateway owner).
- scripts/harness/openapi_contract_gate.py, openapi_profiles.py and knowledge_proxy_contract.py → ask Gateway/OpenAPI owner; the Python/FastAPI provenance adapter is obsolete, while native public-schema completeness remains an explicit separate boundary.
- scripts/harness/prepare_native_gateway_config.py → platform-config-loader (root).
- scripts/harness/runtime_dependency_gate.py → root Make runtime-dependency-gate uses platform-tools python-inventory --require-zero; the old Python-loop AST check is structurally superseded by no repository Python plus the native single-kernel AST gate. This is not an external-interpreter prohibition.
- scripts/inventory/database_policy.py, generate_database_grants.py → migration-authority database-policy / generate-database-grants (auth owner).
- remaining scripts/inventory generators and scripts/core_boundary tools → platform-tools inventory_generators/inventory_* (auth owner).
- scripts/release, scripts/dev, preflight and recovery archive producers → platform-release (root).

## Preserved unverified obligations and external interpreter boundary

- Actual live provider, image, collector, real DB and multi-platform package
  acceptance remain unrun. Never replace them with offline PASS receipts.
- The consolidated native KB migration gate covers five real ignored PG tests;
  specialized historical embedding/artifact assertions are not all recreated.
- Source asyncpg global query tracing and HTTPX monkeypatch CLIENT spans are not
  implemented by the shared native inbound SERVER-span migration. Genuine W3C
  context, sampling, optional OTLP export and request propagation are implemented.
- Windows packaging now has a dedicated portable Rust crate/action; it has not
  been executed on Windows here.
- Capability-worker scientific Python execution and the code-interpreter image
  remain external user-code interpreter dependencies. This lane did not remove
  or relabel them as Rust. Zero repository Python implementations does not mean
  zero external Python execution capability. Root retains that explicit boundary.
