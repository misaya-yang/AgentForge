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
    platform-tools embed-header-check --repo-root "$PROJECT_ROOT"
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
    --entrypoint /usr/local/bin/platform-tools \
    "${NATIVE_FIXTURE_IMAGE:-ai-platform/tools:rust-local}" embed-fixture-server --bind 0.0.0.0:8080 >/dev/null
docker run -d --name "$frontend_container_name" --network "$network" \
    --user 101:101 --read-only --cap-drop ALL --security-opt no-new-privileges \
    --tmpfs /tmp:rw,noexec,nosuid,size=64m,uid=101,gid=101,mode=1770 \
    -p 127.0.0.1::8080 "$image" >/dev/null

port="$(docker port "$frontend_container_name" 8080/tcp | awk -F: 'NR==1 {print $NF}')"
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
