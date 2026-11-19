# Offline native coding fixture check

The frozen fixture is intentionally defective. Do not patch the original repository or change its sealed hashes. `coding-v2-reference.patch` modifies only the two allowed implementation files: largest-remainder allocation with stable ID tie-break, and recursive JSON object key ordering. `patch --dry-run` against the frozen source succeeded (first hunk offset -1, no fuzz).

Parent-controlled serialized commands (substitute the selected isolated output and toolchain paths):

```sh
ROOT=/workspace/scratch/c4693694c197/AI--Platfform
OUT=/workspace/shared/platform-verification/full-rust-20261015/coding-v2-offline-check
mkdir -p "$OUT"
cp -a "$ROOT/tests/fixtures/eval/general_agent/coding_parallel_v2/repository" "$OUT/unsolved"
cp -a "$ROOT/tests/fixtures/eval/general_agent/coding_parallel_v2/repository" "$OUT/reference"
patch -p1 -d "$OUT/reference" < "$ROOT/reports/fixtures/coding-v2-reference.patch"
# Run from each directory so .cargo/config.toml resolves its local vendored dependencies.
# Use the parent's selected Cargo and disk guard; do not run concurrently.
(cd "$OUT/unsolved" && cargo test --offline --locked --no-fail-fast --test allocation --test idempotency --test service)
# Expected: nonzero, two allocation failures; idempotency nested-order failure with tenant-scope pass; service failure.
(cd "$OUT/reference" && cargo test --offline --locked --test allocation --test idempotency --test service)
# Expected: five business tests pass across the three target files.
```

Keep actual outputs and exit statuses. This is native functional fixture verification only: it does not establish restricted-container execution, live agent participation, authenticated host receipts, or benchmark acceptance. Original source integration required a restricted sandbox; that environmental evidence remains unrun.

## Executed after source checkpoint126

2026-10-15 16:10 UTC: root ran the exact offline commands with Cargo1.99.0, locked vendored dependencies and an external target. Original frozen repository remained unchanged.
- Unsolved copy: expected exit101, four named business failures (two allocation, nested-order idempotency, end-to-end retry) and tenant-scope control passed.
- Isolated reference-patched copy: all five tests across three targets passed.
- Evidence: `coding-fixture106.log` in the verification workspace. No Docker/provider/agent was involved; restricted sandbox and live benchmark acceptance remain unverified.
