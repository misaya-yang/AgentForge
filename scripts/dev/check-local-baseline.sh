#!/usr/bin/env bash
# Orchestrate existing gates; retain failures and continue collecting evidence.
set -euo pipefail
BASELINE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$BASELINE_ROOT"
RESULT_DIR="$BASELINE_ROOT/tmp/local-baseline"
mkdir -p "$RESULT_DIR"
printf 'gate\texit_code\tcommand\n' > "$RESULT_DIR/results.tsv"
failures=0
run() {
    local name="$1" code=0 display
    shift
    "$@" > "$RESULT_DIR/$name.log" 2>&1 || code=$?
    printf -v display '%q ' "$@"
    printf '%s\t%s\t%s\n' "$name" "$code" "$display" >> "$RESULT_DIR/results.tsv"
    if [ "$code" -eq 0 ]; then
        printf 'PASS %s\n' "$name"
    else
        printf 'FAIL %s (exit %s; tmp/local-baseline/%s.log)\n' "$name" "$code" "$name"
        failures=$((failures + 1))
    fi
}

run doctor make doctor
run config-example make validate-example-config
run config-local make validate-config
run harness make harness-check
run architecture make architecture-boundary-gate
run core-boundary make core-boundary-gate
run single-instance make single-instance-guard
run runtime-dependency make runtime-dependency-gate
run gateway-kb-boundary make gateway-kb-boundary-gate
run ci-enforcement-selftest make ci-gate-enforcement-selftest
run hygiene make hygiene-check
run shell-syntax bash -c 'for script in scripts/dev/*.sh scripts/new/*.sh; do bash -n "$script" || exit; done'
run python-lint uv run --all-packages --extra dev ruff check \
    scripts/harness/check_harness.py scripts/dev/local-services.py scripts/dev/prepare-tokenizer.py \
    tests/services/knowledge/test_startup_routes_fail_closed.py tests/scripts/test_runtime_script_topology_contract.py \
    tests/scripts/test_validate_env_quickstart.py tests/scripts/test_web_runtime_config_entrypoint.py \
    tests/scripts/test_seed_demo_data.py
run gateway-unit make gateway-unit-gate
run knowledge-unit make kb-unit-gate
run worker-unit uv run --project apps/sdk-agent-worker python -m unittest discover -s apps/sdk-agent-worker/tests
run local-node-unit env PYTHONPATH=apps/local-node/src uv run --all-packages --extra test pytest -q --no-cov apps/local-node/tests
run python-build uv build --all-packages --out-dir tmp/local-baseline/dist
run web-types pnpm -C web type-check
run web-lint pnpm -C web lint
run web-unit pnpm -C web test
run web-i18n pnpm -C web i18n:check
run web-build pnpm -C web build
run native-fmt cargo +1.99.0 fmt --manifest-path rust/platform-native/Cargo.toml --all --check
run native-check cargo +1.99.0 check --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --all-targets
run native-unit cargo +1.99.0 test --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --lib
run native-build cargo +1.99.0 build --locked --manifest-path rust/platform-native/Cargo.toml -p ai-platform-gateway --bin gateway --bin knowledge-service
python3 - "$RESULT_DIR" <<'PY'
import csv
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
with (root / "results.tsv").open() as source:
    rows = list(csv.DictReader(source, delimiter="\t"))
for row in rows:
    row["exit_code"] = int(row["exit_code"])
    row["status"] = "PASS" if row["exit_code"] == 0 else "FAIL"
(root / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
PY
printf '%s failed command(s); full results: tmp/local-baseline/results.json\n' "$failures"
[ "$failures" -eq 0 ]
