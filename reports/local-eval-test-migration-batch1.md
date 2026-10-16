# Local / evaluation test migration, batch 1

Status: authored and formatted; root verification pending. No Cargo, provider, database service, or GUI run by this lane. Python reference tests remain until verification.

## Local acceptance

`platform-local-node/tests/local_migration_parity_acceptance.rs` and
`platform-gateway-integrations/tests/local_migration_parity_pairing.rs` retain these source business assertions:

- `tests/services/assistant/local_os_acceptance/test_action_envelope.py`: canonical digest, argument rejection, absent authority, all 23 signed authority-field mutations, forged and one-use local proof, 3 action-intent and 5 proof-binding mutations, stale target, missing/expired/nonfinite approval, nonfinite envelope, 11 identity values, secret-free digest.
- `test_action_ledger.py`: durable policy receipt, missing authority, permitted/forbidden transitions, pre/post-dispatch disconnect, idempotency, changed intent, all 14 signed-scope replay mutations, envelope digest, identical terminal acknowledgement and conflicting terminal/result rejection, modify/delete/reorder integrity failures, legacy-unbound migration/replay denial, secret redaction across DB/WAL/SHM.
- `test_workspace_adversarial.py`: original JSON path corpus, Unicode normalization, file/directory symlink, hardlink, safe-file-to-symlink replacement, tenant/user binding, capability restrictions, revocation, root replacement.
- `test_pairing_security.py`: single-use challenge, consumed wrong-secret attempt, device-bound credential, private metadata and no key bytes, symlink metadata refusal, absent storage refusal, plaintext legacy refusal. The existing exact PairingManager implementation is exercised, without duplicating a second authority implementation.
- `test_runtime_fail_closed.py`: literal-loopback listener, TLS/credential-free outbound URL, missing Mac backend, non-Mac unsupported health, unavailable driver lease rejection, sandbox denial and secret-like env rejection before receipt/spawn, actual env subprocess receipt excludes ambient canary and contains only the minimal environment. The ambient variable is isolated in a child test process instead of mutating global environment during concurrent Rust tests. It requires `/usr/bin/env` explicitly and never silently skips.
- `conftest.py`: replaced by deterministic native signing/approval/action/temp fixtures. JSON fixtures remain.

Language-specific retirement: Python frozen-dataclass mutation exception and repr implementations do not exist in the native API. Native secret-bearing identity/challenge/credential/action structs intentionally lack Debug, private keys remain private, and signed identity mutation fails authority verification. These mechanism-specific assertions are not represented as runtime behavioral test passes.

Two source compatibility repairs discovered by these assertions are in local-node production: receipt target mismatch returns Stale; old SQLite actions schema gains `envelope_digest=legacy-unbound`, retains readable but unreplayable old receipts, and inserts fresh signed actions using explicit columns. Hash-chain verification still applies before exempting historical unbound rows from unavailable envelope bindings.

Eligibility: all five local source test files can be retired after the listed native targets pass, with the explicit Python-only retirement above. Keep fixtures.

## Evaluation

- `test_exact_output_assertion.py`: all six whole-response comparisons and four invalid assertion values in `platform-eval/tests/eval_migration_parity_exact_output.rs`.
- `test_search_indexes.py`: both original migration declaration assertions in that same target. This preserves the original static SQL contract; it does not claim live database index execution.
- `test_golden_regression_gate.py`: pure evaluation/gate behavior in `eval_migration_parity_golden.rs`; real offline CLI/report/snapshot/provenance behavior in `eval_migration_parity_golden_cli.rs`, with existing `tests/golden.rs` canonical fixture gate coverage. Native provenance checks hash the four actual native grader components, rather than stale Python files.
- Golden facade import identity and monkeypatch propagation are Python-only implementation mechanisms, retired explicitly. Python datetime object acceptance becomes RFC3339 JSON timestamp acceptance (native case included). Nonfinite Python float inputs become rejection at native JSON parse boundary and explicit invalid/null metric/threshold/baseline cases; no invalid value becomes a passing gate.

Current residual before golden whole-file retirement: root verification of all new tests. Golden hard-blocker IDs, case counts, threshold values and evidence tiers are preserved.

## Inventory boundaries

`tests/services/assistant/tools/__init__.py` is only an empty package marker; no behavioral tests exist there. No `tests/unit` files import `local_node`, `ai_platform_client`, or `sdk.python`; SDK worker/bridge suites belong to the SDK service lane. Remaining `tests/services/eval` suites and coding-v1 fixture business assertions are subsequent batches, not declared migrated by this report.
