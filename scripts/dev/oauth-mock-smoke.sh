#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
addr="${CATALOG_DEV_ADDR:-127.0.0.1:8098}"
issuer_port="${OAUTH_MOCK_PORT:-8099}"
database_url="${AI_GATEWAY_CATALOG_DATABASE_URL:-}"
if [[ -z "$database_url" ]]; then
  echo "AI_GATEWAY_CATALOG_DATABASE_URL is required" >&2
  exit 1
fi
cipher="${CATALOG_CIPHER_KEY:-catalog-test-key}"
work="$(mktemp -d)"
cleanup() {
  if [[ -n "${server_pid:-}" ]]; then kill "$server_pid" 2>/dev/null || true; fi
  if [[ -n "${mock_pid:-}" ]]; then kill "$mock_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
python3 - "$issuer_port" <<'PY' &
import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs
port = int(sys.argv[1])
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/.well-known/oauth-authorization-server":
            body = {
                "issuer": f"http://127.0.0.1:{port}",
                "authorization_endpoint": f"http://127.0.0.1:{port}/authorize",
                "token_endpoint": f"http://127.0.0.1:{port}/token",
                "registration_endpoint": f"http://127.0.0.1:{port}/register",
                "client_id_metadata_document_supported": False,
            }
            return self._json(body)
        if parsed.path == "/authorize":
            return self._json({"ok": True, "query": parse_qs(parsed.query)})
        self.send_response(404); self.end_headers()
    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length).decode()
        if self.path == "/register":
            return self._json({"client_id": "mock-client", "client_secret": "mock-client-secret"})
        if self.path == "/token":
            if "plaintext-access-token" in raw:
                return self._json({"error": "refused"})
            return self._json({"access_token": "plaintext-access-token", "token_type": "bearer"})
        self.send_response(404); self.end_headers()
    def _json(self, body):
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
mock_pid=$!
export CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CATALOG_DEV_ADDR="$addr" CATALOG_PUBLIC_BASE="http://$addr" CATALOG_CIPHER_KEY="$cipher" AI_GATEWAY_CATALOG_DATABASE_URL="$database_url"
cargo run --locked --quiet --manifest-path "$root/rust/platform-native/Cargo.toml" -p ai-platform-gateway-foundation --bin catalog_dev_server >"$work/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 180); do
  if curl -sf -o /dev/null "http://$addr/api/v1/catalog"; then break; fi
  if ! kill -0 "$server_pid" 2>/dev/null; then echo "catalog server exited"; cat "$work/server.log"; exit 1; fi
  sleep 1
done
entry="$(curl -sf -X POST "http://$addr/api/v1/catalog" -H 'content-type: application/json' -d "{\"id\":\"mock-oauth\",\"kind\":\"connector\",\"name\":\"Mock OAuth\",\"description\":\"Test provider\",\"category\":\"development\",\"auth_kind\":\"oauth\",\"manifest\":{\"issuer\":\"http://127.0.0.1:${issuer_port}\",\"resource\":\"http://127.0.0.1:${issuer_port}/api\"}}")"
entry_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$entry")"
started="$(curl -sf -X POST "http://$addr/api/v1/connections/${entry_id}/oauth/start" -H 'content-type: application/json' -d '{}')"
python3 - "$started" "$issuer_port" "$addr" <<'PY'
import json, sys, urllib.error, urllib.parse, urllib.request
started = json.loads(sys.argv[1])
issuer_port, addr = sys.argv[2], sys.argv[3]
query = urllib.parse.parse_qs(urllib.parse.urlparse(started["authorization_url"]).query)
state = query["state"][0]
url = f"http://{addr}/api/v1/connections/oauth/callback?code=mock-code&state={urllib.parse.quote(state)}&iss={urllib.parse.quote('http://127.0.0.1:' + issuer_port)}"
class Stop(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None
opener = urllib.request.build_opener(Stop)
try:
    response = opener.open(url)
except urllib.error.HTTPError as error:
    response = error
if response.status not in (200, 303):
    raise SystemExit(f"callback status {response.status}")
PY
status="$(curl -sf "http://$addr/api/v1/connections")"
python3 - "$status" "$entry_id" <<'PY'
import json, sys
body, entry = json.loads(sys.argv[1]), sys.argv[2]
rows = [row for row in body["connections"] if row["entry_id"] == entry]
assert rows and rows[0]["status"] == "connected", body
assert rows[0]["has_credential"] is True
assert "plaintext-access-token" not in json.dumps(body)
print("connection connected")
PY
python3 - "$database_url" <<'PY'
import subprocess, sys
url = sys.argv[1]
query = "SELECT credential_ciphertext FROM gateway.catalog_connections UNION ALL SELECT code_verifier_ciphertext FROM gateway.catalog_oauth_states"
result = subprocess.run(["psql", url, "-tA", "-v", "ON_ERROR_STOP=1", "-c", query], capture_output=True, text=True)
if result.returncode != 0:
    raise SystemExit("database query failed")
rows = [line for line in result.stdout.splitlines() if line.strip()]
blob = "\n".join(rows)
if "plaintext-access-token" in blob or "mock-client-secret" in blob:
    raise SystemExit("plaintext secret stored")
if not rows or any(not line.startswith("enc:") for line in rows):
    raise SystemExit("ciphertext missing enc: prefix")
print("database ciphertext only")
PY
echo "oauth-mock-smoke ok"
