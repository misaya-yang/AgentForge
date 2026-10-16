# Native evaluation tools

This crate provides five native CLI binaries preserving the evaluation tool
contracts and fixture formats:

- `eval_golden validate|summarize|gate|candidate|import|export|live`
- `eval_rag validate|gate`
- `regen_rag_observations record|verify`
- `seed_kb_golden_set`
- `assistant_runtime_regression gate|summary`

Example invocations from the repository root, after building the native tools:

```sh
eval_golden gate tests/fixtures/eval/golden/assistant_regression_v1.jsonl \
  --observations tests/fixtures/eval/observations/assistant_regression_v1.jsonl
eval_rag gate tests/fixtures/eval/rag/golden/rag_regression_v1.jsonl \
  --observations tests/fixtures/eval/rag/observations/rag_regression_v1.jsonl
seed_kb_golden_set --check
regen_rag_observations verify
```

RAG recording only admits retrieval-only cases and writes no partial observations.
It does not synthesize answers or judge evidence. Recorded answer gating requires
explicit opt-in and a policy manifest whose bytes match `RAG_JUDGE_POLICY_SHA256`.
Bootstrap confidence intervals retain the original MT19937 seed and sampling
algorithm. Fixture hashes are computed from the same byte snapshot parsed.

Golden gates preserve assertion, tool lifecycle, stateful plan/budget/HITL,
compaction and security evidence requirements. Native provenance records hashes
of the native grader sources. Candidate mode consumes recorded producer artifacts;
it does not invoke a model. Live mode starts one candidate run, polls it, and uses
the server's baseline comparison gate. Mutating admission requests are not retried.

The runtime regression aggregator preserves group identities and all-groups-pass
criteria. It runs explicit native gateway integration targets and fails on missing
or zero-test suites; mere successful process exit is not coverage. The required
targets are `runtime_v2_turn_contract`, `runtime_control_plane`,
`runtime_capability_contract`, and `runtime_eval_contract`. Their source-test port
and acceptance are independently tracked; the runner does not waive missing tests.

No binary launches or imports Python. Existing JSONL fixtures are read unchanged.
Platform owners serialize Cargo gates; do not run the aggregator concurrently with
another workspace build.

## Additional migrated tools

- `agent_studio_regression`: required phase gates, source snapshot hashing, log hashes,
  and skip-as-failure behavior. Its historical phase test matrices must each be
  mapped to substantive native targets before final manifest cutover.
- `eval_general_agent_failure_recovery`: original four-case strict suite, explicit
  offline fixture mode and live terminal-event contract.
- `validate_real_finance_eval`: independent arithmetic, citations, source hashes,
  specialist overlap/read-only boundaries, and three-run scoring.
- `assistant_ttft_benchmark`: actual Gateway adapter, timing log reconciliation,
  original nearest-rank/bootstrap statistics and all-trial certification rules.
- `real_agent_scenario_runner collect|validate|merge`: strict scenario contracts,
  factual SSE collection, private raw events, independent collector/golden keys,
  source/plugin binding, anti-replay checks and raw-event reconstruction. Merge
  re-executes deterministic golden and execution checks before sealing.
- `verify_coding_parallel_fixture`: fresh live evidence, strict two-file patch
  scope, changed-line limits, isolated native Cargo tests, and independently keyed
  host receipts. It never executes a candidate directly on the host.

The Rust coding fixture is `coding_parallel_v2`. It preserves the original two
intentional business defects and five acceptance scenarios. Its image policy is
currently unbound and fails closed until release provisioning supplies a verified
native image digest and locked offline dependencies. Four fixture tests are
expected to fail before repair. Old Python fixture receipts cannot certify v2.
The fixture is intentionally outside the service Cargo workspace.

Collector and host-test tools perform real external work only when explicitly
invoked. Source migration and offline unit tests are not live acceptance evidence.
Private report writes use mode 0600; TTFT/resource reports atomically replace files
using exclusive temporary inodes. Scenario and coding HMAC keys are loaded only
at command execution and are never included in reports.

`judge_real_agent_receipts` replaces the sealed real-agent release judge. Supply
matched repeated `--scenarios`, `--receipts`, and
`--expected-receipt-sha256` arguments, plus `--output`; coding additionally needs
`--coding-host-test-receipt`. `--repo-root` selects repository-owned plugin and
release-policy artifacts. The default is the current repository directory.

Collector/golden/coding verification keys and the independent suite nonce retain
their existing environment names. DeepSeek credentials are read only after all
input and release-manifest checks pass; calling this command on valid inputs
performs two real paid provider judgments per trial. Ordinary unit tests make no
provider calls. Immutable old release manifests are not rewritten to accommodate
native coding-v2 fixture changes: reviewed new release policy/image pins are
required before those release runs can pass.
