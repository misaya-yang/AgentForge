#!/usr/bin/env bash
# Existing production assets and benchmark services are required. No startup or env-file reads.
set -euo pipefail
set +x
root=$(cd "$(dirname "$0")/../.." && pwd -P)
: "${E2E_BASE_URL:?Set the URL of an existing production frontend}"
: "${PERF_PLATFORM_API_KEY:?Export a benchmark platform API key}"
command -v oha >/dev/null || { echo 'oha is required' >&2; exit 1; }
command -v perf-dot-1113 >/dev/null || { echo 'Build perf-dot-1113 using scripts/new/native-container.sh and install the controlled command shim first' >&2; exit 1; }
(cd "$root/web" && pnpm exec playwright test --config playwright.perf.config.ts e2e/perf-cwv.spec.ts)
report_date=${PERF_REPORT_DATE:-$(TZ=America/New_York date +%F)}
exec perf-dot-1113 "$root" "$report_date"
