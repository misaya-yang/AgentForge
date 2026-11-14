#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
addr="${CATALOG_DEV_ADDR:-127.0.0.1:8096}"
bearer="${CATALOG_DEV_BEARER:-catalog-dev-bearer}"
title="M2 page"
knowledge_port="${KNOWLEDGE_STUB_PORT:-8097}"
work="$(mktemp -d)"
cleanup() {
  if [[ -n "${server_pid:-}" ]]; then kill "$server_pid" 2>/dev/null || true; fi
  if [[ -n "${knowledge_pid:-}" ]]; then kill "$knowledge_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
python3 - "$knowledge_port" "$title" <<'PY' &
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
port, title = int(sys.argv[1]), sys.argv[2]
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = {"items": [{"item_id": "page-m2", "title": title, "space_id": "space-m2"}]}
        raw = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)
    def log_message(self, *_args):
        return
ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
knowledge_pid=$!
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CATALOG_DEV_ADDR="$addr" CATALOG_DEV_BEARER="$bearer" CATALOG_PUBLIC_BASE="http://$addr" KNOWLEDGE_BASE_URL="http://127.0.0.1:$knowledge_port"
cargo run --locked --quiet --manifest-path "$root/rust/platform-native/Cargo.toml" -p ai-platform-gateway-foundation --bin catalog_dev_server >"$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 120); do
  if curl -sf -o /dev/null "http://$addr/.well-known/oauth-protected-resource/mcp"; then break; fi
  if ! kill -0 "$server_pid" 2>/dev/null; then echo "catalog server exited"; cat "$work/server.log"; exit 1; fi
  sleep 1
done
client="${MCP_CLIENT_HOME:-/tmp/agentforge-mcp-client}"
if [[ ! -d "$client/node_modules/@modelcontextprotocol/sdk" ]]; then
  mkdir -p "$client"
  npm --prefix "$client" init -y >/dev/null
  npm --prefix "$client" install @modelcontextprotocol/sdk@1.32.2 --silent
fi
MCP_URL="http://$addr/mcp" MCP_BEARER="$bearer" MCP_PAGE_TITLE="$title" MCP_SDK_ROOT="$client/node_modules/@modelcontextprotocol/sdk" node "$root/scripts/dev/mcp-client.mjs"
