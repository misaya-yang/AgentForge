#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/repository_root.sh"
repo_root="$(platform_repository_root)"
cd "$repo_root"
image="${AGENT_CAPABILITY_WORKER_IMAGE:-${AI_PLATFORM_CAPABILITY_WORKER_IMAGE:-}}"
if [[ -z "$image" ]]; then
    echo "ERROR: AGENT_CAPABILITY_WORKER_IMAGE is required" >&2
    exit 2
fi
docker image inspect "$image" >/dev/null 2>&1 || {
    echo "ERROR: capability worker image is not present" >&2
    exit 2
}

suffix="$(python3 -c 'import uuid; print(uuid.uuid4().hex[:12])')"
network="capability-worker-smoke-${suffix}"
postgres="capability-worker-pg-${suffix}"
worker="capability-worker-${suffix}"
password="$(openssl rand -hex 24)"
token="$(openssl rand -hex 32)"
lease_secret="$(openssl rand -hex 32)"
proof_secret="$(openssl rand -hex 32)"
tenant_id="tenant-${suffix}"
user_id="user-${suffix}"
session_id="session-${suffix}"
run_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
snapshot_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
runtime_thread_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
model_lease_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
scratch="$(mktemp -d /tmp/ai-platform-capability-worker-smoke.XXXXXX)"
host_port="$(python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"

cleanup() {
    status=$?
    if [[ "$status" -ne 0 ]] && docker inspect "$worker" >/dev/null 2>&1; then
        docker logs --tail 120 "$worker" >&2 || true
    fi
    if [[ "$status" -ne 0 ]] && docker inspect "$postgres" >/dev/null 2>&1; then
        docker logs --tail 120 "$postgres" >&2 || true
    fi
    docker rm -f "$worker" "$postgres" >/dev/null 2>&1 || true
    docker network rm "$network" >/dev/null 2>&1 || true
    rm -rf -- "$scratch"
    return "$status"
}
trap cleanup EXIT

# Use a dedicated bridge network. Docker Desktop does not expose host-published
# ports from an internal network, while the smoke client deliberately connects
# through a loopback-only random host port.
docker network create "$network" >/dev/null
docker run -d --rm \
    --name "$postgres" \
    --network "$network" \
    -e POSTGRES_USER=smoke \
    -e "POSTGRES_PASSWORD=$password" \
    -e POSTGRES_DB=smoke \
    postgres:16-alpine >/dev/null
for _ in $(seq 1 60); do
    if docker exec "$postgres" pg_isready -U smoke -d smoke >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
docker exec "$postgres" pg_isready -U smoke -d smoke >/dev/null

docker exec -i "$postgres" psql -v ON_ERROR_STOP=1 -U smoke -d smoke >/dev/null <<'SQL'
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE TABLE assistant_runs (
    run_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'running',
    engine VARCHAR(32) NOT NULL DEFAULT 'agent_runtime',
    UNIQUE (run_id, tenant_id, user_id, session_id)
);
CREATE TABLE assistant_tool_approvals (
    approval_id UUID PRIMARY KEY,
    tenant_id VARCHAR(64) NOT NULL,
    user_id VARCHAR(64) NOT NULL,
    session_id VARCHAR(100) NOT NULL,
    run_id UUID,
    tool_name VARCHAR(160) NOT NULL,
    arguments JSONB NOT NULL DEFAULT '{}'::jsonb,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE assistant_runtime_snapshots (
    snapshot_id UUID PRIMARY KEY,
    runtime_thread_id UUID NOT NULL,
    run_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    capability_revision BIGINT NOT NULL,
    snapshot JSONB NOT NULL,
    expires_at TIMESTAMPTZ
);
CREATE TABLE assistant_runtime_model_leases (
    lease_id UUID PRIMARY KEY,
    snapshot_id UUID NOT NULL,
    run_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL,
    capability_revision BIGINT NOT NULL,
    status VARCHAR(32) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE assistant_runtime_snapshot_revocations (
    snapshot_id UUID PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL,
    user_id VARCHAR(255) NOT NULL,
    session_id VARCHAR(255) NOT NULL
);
SQL
docker exec -i "$postgres" psql -v ON_ERROR_STOP=1 -U smoke -d smoke \
    < "$repo_root/database/migrations/096_agent_capability_executions.sql" >/dev/null
docker exec "$postgres" psql -v ON_ERROR_STOP=1 -U smoke -d smoke -c \
    "INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id) VALUES ('$run_id','$tenant_id','$user_id','$session_id')" \
    >/dev/null
docker exec "$postgres" psql -v ON_ERROR_STOP=1 -U smoke -d smoke -c \
    "INSERT INTO assistant_runtime_snapshots(snapshot_id,runtime_thread_id,run_id,tenant_id,user_id,session_id,capability_revision,snapshot,expires_at) VALUES ('$snapshot_id','$runtime_thread_id','$run_id','$tenant_id','$user_id','$session_id',1,'{\"readonly_capabilities\":{\"items\":[],\"capability_allowlist\":[{\"type\":\"tool\",\"name\":\"platform.read_fixture\",\"id\":\"platform.read_fixture\",\"version\":\"1\",\"schema_hash\":\"sha256:b6b7a41b56d58389b6f729bbe9911814887cf81fc0b18765eae1b501d782d9ef\"}]}}'::jsonb,NOW()+INTERVAL '10 minutes')" \
    >/dev/null
docker exec "$postgres" psql -v ON_ERROR_STOP=1 -U smoke -d smoke -c \
    "INSERT INTO assistant_runtime_model_leases(lease_id,snapshot_id,run_id,tenant_id,user_id,session_id,capability_revision,status,expires_at) VALUES ('$model_lease_id','$snapshot_id','$run_id','$tenant_id','$user_id','$session_id',1,'active',NOW()+INTERVAL '10 minutes')" \
    >/dev/null

start_worker() {
    docker run -d --rm \
        --name "$worker" \
        --network "$network" \
        -p "127.0.0.1:${host_port}:8095/tcp" \
        --memory 256m \
        --cpus 1 \
        -e "AI_PLATFORM_INTERNAL_TOKEN=$token" \
        -e "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET=$lease_secret" \
        -e "AI_PLATFORM_CAPABILITY_PROOF_SECRET=$proof_secret" \
        -e AI_PLATFORM_CAPABILITY_WORKER_FIXTURES_ENABLED=true \
        -e POSTGRES_HOST="$postgres" \
        -e POSTGRES_USER=smoke \
        -e "POSTGRES_PASSWORD=$password" \
        -e POSTGRES_DB=smoke \
        -e AI_PLATFORM_CAPABILITY_WORKER_DB_POOL_MAX_SIZE=2 \
        -e AI_PLATFORM_CAPABILITY_WORKER_BIND=0.0.0.0:8095 \
        "$image" >/dev/null
    for _ in $(seq 1 60); do
        if docker exec "$worker" ai-platform-capability-worker-health >/dev/null 2>&1; then
            break
        fi
        sleep 1
    done
    docker exec "$worker" ai-platform-capability-worker-health >/dev/null
}

start_worker

CAPABILITY_SMOKE_TOKEN="$token" \
CAPABILITY_SMOKE_LEASE_SECRET="$lease_secret" \
CAPABILITY_SMOKE_TENANT="$tenant_id" \
CAPABILITY_SMOKE_USER="$user_id" \
CAPABILITY_SMOKE_SESSION="$session_id" \
CAPABILITY_SMOKE_RUN="$run_id" \
CAPABILITY_SMOKE_URL="http://127.0.0.1:${host_port}" \
CAPABILITY_SMOKE_SCRATCH="$scratch" \
uv run --all-packages --extra test python - <<'PY'
import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from src.services.agent_runtime.capability_leases import CapabilityEffect, CapabilityLeaseIssuer

base = os.environ["CAPABILITY_SMOKE_URL"]
token = os.environ["CAPABILITY_SMOKE_TOKEN"]
tenant = os.environ["CAPABILITY_SMOKE_TENANT"]
user = os.environ["CAPABILITY_SMOKE_USER"]
session = os.environ["CAPABILITY_SMOKE_SESSION"]
run_id = os.environ["CAPABILITY_SMOKE_RUN"]
scratch = Path(os.environ["CAPABILITY_SMOKE_SCRATCH"])
issuer = CapabilityLeaseIssuer(os.environ["CAPABILITY_SMOKE_LEASE_SECRET"], ttl_ms=120_000)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
headers = {
    "content-type": "application/json",
    "x-ai-platform-internal-token": token,
    "x-ai-tenant-id": tenant,
    "x-ai-user-id": user,
    "x-ai-session-id": session,
}


def call(method, path, body=None, request_headers=None):
    payload = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    request = urllib.request.Request(
        base + path,
        data=payload,
        method=method,
        headers=request_headers or headers,
    )
    try:
        with opener.open(request, timeout=5) as response:
            raw = response.read()
            try:
                return response.status, json.loads(raw)
            except json.JSONDecodeError as error:
                raise AssertionError(
                    f"non-JSON response status={response.status} body={raw[:512]!r}"
                ) from error
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw)
        except json.JSONDecodeError as decode_error:
            raise AssertionError(
                f"non-JSON error status={error.code} body={raw[:512]!r}"
            ) from decode_error


catalog_status, catalog = call(
    "POST",
    "/internal/v2/capabilities/catalog",
    {
        "schema_version": "capability-catalog/v2",
        "tenant_id": tenant,
        "user_id": user,
        "session_id": session,
        "capability_revision": 1,
    },
)
assert catalog_status == 200
assert {item["id"] for item in catalog["capabilities"]} >= {
    "platform.echo",
    "platform.read_fixture",
    "search_knowledge_base",
}


def execution_request(call_id, arguments, idempotency_key):
    lease = issuer.issue(
        tenant_id=tenant,
        user_id=user,
        session_id=session,
        run_id=run_id,
        tool_call_id=call_id,
        attempt_id="attempt-1",
        capability_id="platform.read_fixture",
        capability_revision=1,
        arguments=arguments,
        effect=CapabilityEffect.READ,
    )
    return {
        "schema_version": "capability-execution/v2",
        "lease": lease.to_dict(),
        "idempotency_key": idempotency_key,
        "descriptor": next(
            item
            for item in catalog["capabilities"]
            if item["id"] == "platform.read_fixture"
        ),
        "arguments": arguments,
    }


request = execution_request("call-basic", {"key": "basic"}, "idem-basic")
status, execution = call("POST", "/internal/v2/capabilities/executions", request)
assert status == 202, (status, execution)
replay_status, replay = call("POST", "/internal/v2/capabilities/executions", request)
assert replay_status == 200 and replay["execution_id"] == execution["execution_id"]
conflict_request = execution_request(
    "call-basic", {"key": "changed"}, "idem-basic"
)
conflict_status, _ = call(
    "POST", "/internal/v2/capabilities/executions", conflict_request
)
assert conflict_status == 409

forged_headers = dict(headers)
forged_headers["x-ai-tenant-id"] = "tenant-forged"
forged_status, _ = call(
    "POST",
    "/internal/v2/capabilities/executions",
    request,
    forged_headers,
)
assert forged_status == 403

after = 0
terminal = []
for _ in range(100):
    event_status, page = call(
        "GET",
        f"/internal/v2/capabilities/executions/{execution['execution_id']}/events?after_sequence={after}",
    )
    assert event_status == 200 and len(page["events"]) <= 1
    if page["events"]:
        event = page["events"][0]
        assert event["sequence"] > after
        after = event["sequence"]
        if event["status"] in {"succeeded", "failed", "cancelled", "timeout", "side_effect_unknown"}:
            terminal.append(event)
            break
    time.sleep(0.02)
assert len(terminal) == 1 and terminal[0]["status"] == "succeeded"

cancel_request = execution_request(
    "call-cancel", {"key": "cancel", "delay_ms": 5_000}, "idem-cancel"
)
cancel_status, cancel_execution = call(
    "POST", "/internal/v2/capabilities/executions", cancel_request
)
assert cancel_status == 202
cancelled_status, cancelled = call(
    "POST",
    f"/internal/v2/capabilities/executions/{cancel_execution['execution_id']}:cancel",
)
assert cancelled_status == 200 and cancelled["status"] == "cancelled"

recovery_request = execution_request(
    "call-recover", {"key": "recover", "delay_ms": 5_000}, "idem-recover"
)
recovery_status, recovery = call(
    "POST", "/internal/v2/capabilities/executions", recovery_request
)
assert recovery_status == 202
scratch.joinpath("recovery-request.json").write_text(json.dumps(recovery_request))
scratch.joinpath("recovery-execution-id").write_text(recovery["execution_id"])
print("capability-worker-smoke: catalog, idempotency, scope, cursor and cancel passed")
PY

docker rm -f "$worker" >/dev/null
start_worker

CAPABILITY_SMOKE_TOKEN="$token" \
CAPABILITY_SMOKE_TENANT="$tenant_id" \
CAPABILITY_SMOKE_USER="$user_id" \
CAPABILITY_SMOKE_SESSION="$session_id" \
CAPABILITY_SMOKE_URL="http://127.0.0.1:${host_port}" \
CAPABILITY_SMOKE_SCRATCH="$scratch" \
uv run --all-packages --extra test python - <<'PY'
import json
import os
import time
import urllib.request
from pathlib import Path

base = os.environ["CAPABILITY_SMOKE_URL"]
scratch = Path(os.environ["CAPABILITY_SMOKE_SCRATCH"])
request_body = json.loads(scratch.joinpath("recovery-request.json").read_text())
execution_id = scratch.joinpath("recovery-execution-id").read_text()
headers = {
    "content-type": "application/json",
    "x-ai-platform-internal-token": os.environ["CAPABILITY_SMOKE_TOKEN"],
    "x-ai-tenant-id": os.environ["CAPABILITY_SMOKE_TENANT"],
    "x-ai-user-id": os.environ["CAPABILITY_SMOKE_USER"],
    "x-ai-session-id": os.environ["CAPABILITY_SMOKE_SESSION"],
}
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def call(method, path, body=None):
    payload = None if body is None else json.dumps(body, separators=(",", ":")).encode()
    request = urllib.request.Request(base + path, data=payload, method=method, headers=headers)
    with opener.open(request, timeout=5) as response:
        return response.status, json.load(response)


status, replay = call("POST", "/internal/v2/capabilities/executions", request_body)
assert status == 200 and replay["execution_id"] == execution_id
# The durable read dispatch lease is 30 seconds. A replacement worker must not
# overtake a still-live predecessor, so crash recovery waits through that
# bounded lease before reclaiming the read.
for _ in range(800):
    status, current = call(
        "GET", f"/internal/v2/capabilities/executions/{execution_id}"
    )
    assert status == 200
    if current["status"] in {"succeeded", "failed", "cancelled", "timeout", "side_effect_unknown"}:
        assert current["status"] == "succeeded"
        break
    time.sleep(0.05)
else:
    raise AssertionError("worker restart did not recover read execution")

after = 0
terminal_events = []
for _ in range(32):
    status, page = call(
        "GET",
        f"/internal/v2/capabilities/executions/{execution_id}/events?after_sequence={after}",
    )
    assert status == 200 and len(page["events"]) <= 1
    if not page["events"]:
        break
    event = page["events"][0]
    assert event["sequence"] > after
    after = event["sequence"]
    if event["status"] in {
        "succeeded",
        "failed",
        "cancelled",
        "timeout",
        "side_effect_unknown",
    }:
        terminal_events.append(event)
assert len(terminal_events) == 1
assert terminal_events[0]["status"] == "succeeded"
print("capability-worker-smoke: restart recovery passed")
PY
