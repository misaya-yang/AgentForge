#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

usage() {
    echo "Usage: scripts/new/test-agent-embed-headers.sh --config-only|--built-image"
}

mode="${1:-}"
if [ "$mode" != "--config-only" ] && [ "$mode" != "--built-image" ]; then
    usage
    exit 2
fi

check_config() {
    python3 - "$PROJECT_ROOT" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
nginx = (root / "web/nginx.conf").read_text(encoding="utf-8")
helm = (root / "deploy/helm/ai-gateway/templates/frontend-configmap.yaml").read_text(encoding="utf-8")
ingress = (root / "deploy/helm/ai-gateway/templates/ingress.yaml").read_text(encoding="utf-8")
main = (root / "src/main.py").read_text(encoding="utf-8")
security = (root / "src/core/middleware/_streaming/security_headers.py").read_text(encoding="utf-8")
agent_public = (root / "src/api/v1/agent_public.py").read_text(encoding="utf-8")

def location_block(source: str, marker: str) -> str:
    start = source.index(marker)
    brace = source.index("{", start)
    depth = 0
    for index in range(brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start:index + 1]
    raise AssertionError(f"unterminated block: {marker}")

for name, source in (("web/nginx.conf", nginx), ("Helm frontend", helm)):
    block = location_block(source, "location /embed/agents/")
    assert "proxy_pass" in block, f"{name}: Embed is not dynamic"
    assert "X-Frame-Options" not in block, f"{name}: Embed inherits explicit XFO"
    assert "Content-Security-Policy" not in block, f"{name}: static CSP replaces Gateway CSP"
    assert "add_header X-Content-Type-Options" in block, f"{name}: inheritance reset missing"

assert 'add_header X-Frame-Options "SAMEORIGIN" always;' in nginx
assert "frame-ancestors 'self'" in nginx
assert 'add_header X-Frame-Options "SAMEORIGIN" always;' in helm
assert "frame-ancestors 'self'" in helm
assert "- path: /embed/agents" in ingress and "-gateway" in ingress
assert "app.include_router(agent_embed_document_router)" in main
assert '_EMBED_PREFIX = "/embed/agents/"' in security
assert "allow_embed_frame = path.startswith(_EMBED_PREFIX)" in security
assert 'response.headers["Content-Security-Policy"]' in agent_public
assert "frame-ancestors {ancestors}" in agent_public
print("Agent Embed config/header contract passed")
PY
}

check_config
if [ "$mode" = "--config-only" ]; then
    exit 0
fi

require_docker
assert_compose_owner "$PROJECT_ROOT"

suffix="$$"
image="agent-studio-web-header-test:$suffix"
network="agent-studio-header-test-$suffix"
gateway_container_name="agent-studio-header-gateway-$suffix"
frontend_container_name="agent-studio-header-frontend-$suffix"

cleanup() {
    docker rm -f "$frontend_container_name" "$gateway_container_name" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker build --load -t "$image" "$PROJECT_ROOT/web"
docker network create "$network" >/dev/null
docker run -d --name "$gateway_container_name" --network "$network" --network-alias gateway \
    -v "$PROJECT_ROOT/tests/deployment/agent_embed_header_fixture.py:/fixture.py:ro" \
    "${PYTHON_FIXTURE_IMAGE:-python:3.12-slim-bookworm}" python /fixture.py >/dev/null
docker run -d --name "$frontend_container_name" --network "$network" \
    -p 127.0.0.1::80 "$image" >/dev/null

port="$(docker port "$frontend_container_name" 80/tcp | awk -F: 'NR==1 {print $NF}')"
base="http://127.0.0.1:$port"
for _attempt in $(seq 1 30); do
    if curl -fsS "$base/health" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

hosted_headers="$(curl -fsSI "$base/a/header-test" 2>/dev/null || curl -fsS -D - -o /dev/null "$base/a/header-test")"
echo "$hosted_headers" | grep -qi '^X-Frame-Options: SAMEORIGIN'
echo "$hosted_headers" | grep -qi "frame-ancestors 'self'"

embed_headers="$(curl -fsS -D - -o /dev/null -H 'Origin: https://allowed.example' "$base/embed/agents/header-test")"
if echo "$embed_headers" | grep -qi '^X-Frame-Options:'; then
    log_error "Dedicated Embed response unexpectedly contains X-Frame-Options"
    exit 1
fi
echo "$embed_headers" | grep -qi 'frame-ancestors https://allowed.example'
echo "$embed_headers" | grep -qi '^Cache-Control: no-store'

log_success "Built frontend dynamic Embed and Hosted header smoke passed"
