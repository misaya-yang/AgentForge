#!/usr/bin/env bash
# PPR-00 baseline reconciliation reader (read-only).
# Usage: read-gateway-timing.sh <since-ISO-8601>
# Wired into: uv run python scripts/assistant_ttft_benchmark.py \
#   --timing-log-command "bash deploy/runbooks/platform-plane-restructure/tools/read-gateway-timing.sh"
set -euo pipefail
cd "$(dirname "$0")/../../../.."
docker compose logs --no-color --since "$1" gateway
