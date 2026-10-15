#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WEB_DIR="$ROOT_DIR/web"
VERIFY_SCRIPT="$ROOT_DIR/scripts/dev/verify_local_stack.py"

ensure_e2e_env() {
  export E2E_API_URL="${E2E_API_URL:-http://127.0.0.1:8080}"
  export E2E_BASE_URL="${E2E_BASE_URL:-http://127.0.0.1:8081}"
  export E2E_KNOWLEDGE_URL="${E2E_KNOWLEDGE_URL:-http://127.0.0.1:8092}"
}

run_verify() {
  ensure_e2e_env
  cd "$ROOT_DIR"
  exec python3 "$VERIFY_SCRIPT" --api-url "$E2E_API_URL"
}

run_tests() {
  ensure_e2e_env
  # global.setup defaults to existing-account mode. Never opt into provisioning here.
  cd "$ROOT_DIR"
  if [ "${1:-}" = "--" ]; then
    shift
  fi
  exec pnpm -C "$WEB_DIR" exec playwright test \
    --config playwright.live.config.ts --workers="${E2E_WORKERS:-1}" "$@"
}

case "${1:-test}" in
  verify)
    shift || true
    run_verify "$@"
    ;;
  test)
    shift || true
    run_tests "$@"
    ;;
  *)
    echo "Usage: $0 [verify|test] [playwright args...]" >&2
    exit 2
    ;;
esac
