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

suffix="$(platform-tools uuid --short12)"
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
run_id="$(platform-tools uuid)"
snapshot_id="$(platform-tools uuid)"
runtime_thread_id="$(platform-tools uuid)"
model_lease_id="$(platform-tools uuid)"
scratch="$(mktemp -d /tmp/ai-platform-capability-worker-smoke.XXXXXX)"
host_port="$(platform-tools free-port)"

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
platform-tools capability-worker-smoke basic

docker rm -f "$worker" >/dev/null
start_worker

CAPABILITY_SMOKE_TOKEN="$token" \
CAPABILITY_SMOKE_TENANT="$tenant_id" \
CAPABILITY_SMOKE_USER="$user_id" \
CAPABILITY_SMOKE_SESSION="$session_id" \
CAPABILITY_SMOKE_URL="http://127.0.0.1:${host_port}" \
CAPABILITY_SMOKE_SCRATCH="$scratch" \
platform-tools capability-worker-smoke recovery
