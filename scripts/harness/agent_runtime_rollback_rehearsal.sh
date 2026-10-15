#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
env_file="${ENV_FILE:-$repo_root/.env}"
bundle_path="${ROLLBACK_BUNDLE:-$repo_root/deploy/runbooks/agent-runtime-full-rust-cutover/rollback-bundle.json}"
report_path="${ROLLBACK_REPORT:-$repo_root/reports/agent-runtime/rollback-rehearsal-latest.json}"
credential_file="${ROLLBACK_E2E_CREDENTIAL_FILE:-$repo_root/web/.playwright/e2e-user.json}"
project_name="${COMPOSE_PROJECT_NAME:-ai-gateway}"

if [[ ! -f "$env_file" || ! -f "$bundle_path" || ! -f "$credential_file" ]]; then
    echo "ERROR: rollback rehearsal requires .env, rollback bundle, and the ignored E2E credential file" >&2
    exit 2
fi

env_value() {
    python3 - "$env_file" "$1" <<'PY'
import ast
import sys

path, key = sys.argv[1:]
for raw_line in open(path, encoding="utf-8"):
    line = raw_line.strip()
    if not line or line.startswith("#") or "=" not in line:
        continue
    candidate, value = line.split("=", 1)
    if candidate.strip() != key:
        continue
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        try:
            value = ast.literal_eval(value)
        except (SyntaxError, ValueError):
            value = value[1:-1]
    print(value)
    break
PY
}

current_runtime_image="$(env_value AI_PLATFORM_AGENT_RUNTIME_IMAGE)"
current_worker_image="$(env_value AGENT_CAPABILITY_WORKER_IMAGE)"
current_kernel_revision="$(env_value AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION)"
postgres_user="$(env_value POSTGRES_USER)"
postgres_db="$(env_value POSTGRES_DB)"
current_runtime_image="${current_runtime_image:?AI_PLATFORM_AGENT_RUNTIME_IMAGE is required}"
current_worker_image="${current_worker_image:?AGENT_CAPABILITY_WORKER_IMAGE is required}"
current_kernel_revision="${current_kernel_revision:?AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION is required}"
postgres_user="${postgres_user:-postgres}"
postgres_db="${postgres_db:-gateway}"
rollback_commit="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["git_commit"])' "$bundle_path")"
rollback_compose_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["compose_file_sha256"])' "$bundle_path")"
rollback_resolved_compose_sha="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["resolved_compose_sha256"])' "$bundle_path")"

bundle_image_field() {
    python3 - "$bundle_path" "$1" "$2" <<'PY'
import json
import sys

path, service, field = sys.argv[1:]
matches = [item for item in json.load(open(path, encoding="utf-8"))["images"] if item["service"] == service]
if len(matches) != 1:
    raise SystemExit(f"rollback bundle must contain exactly one image for {service}")
print(matches[0][field])
PY
}

rollback_runtime_image="${ROLLBACK_RUNTIME_IMAGE:-$(bundle_image_field agent-runtime reference)}"
rollback_assistant_image="${ROLLBACK_ASSISTANT_IMAGE:-$(bundle_image_field assistant-service reference)}"
rollback_gateway_image="${ROLLBACK_GATEWAY_IMAGE:-$(bundle_image_field gateway reference)}"
rollback_frontend_image="${ROLLBACK_FRONTEND_IMAGE:-$(bundle_image_field frontend reference)}"
rollback_knowledge_image="${ROLLBACK_KNOWLEDGE_IMAGE:-$(bundle_image_field knowledge-service reference)}"
rollback_postgres_image="${ROLLBACK_POSTGRES_IMAGE:-$(bundle_image_field postgres reference)}"
rollback_redis_image="${ROLLBACK_REDIS_IMAGE:-$(bundle_image_field redis reference)}"
rollback_qdrant_image="${ROLLBACK_QDRANT_IMAGE:-$(bundle_image_field qdrant reference)}"
rollback_kernel_revision="${ROLLBACK_KERNEL_REVISION:-93c54bca38996b56d344a2ca65f01627b1953b27+c3add32732c2}"

for image in "$current_runtime_image" "$current_worker_image"; do
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        echo "ERROR: required rollback rehearsal image is missing: $image" >&2
        exit 2
    fi
done

verify_bundle_image() {
    local service="$1" image="$2" expected actual
    expected="$(bundle_image_field "$service" image_id)"
    if ! actual="$(docker image inspect "$image" --format '{{.Id}}' 2>/dev/null)"; then
        echo "ERROR: required rollback image is missing: $image" >&2
        exit 2
    fi
    if [[ "$actual" != "$expected" ]]; then
        echo "ERROR: rollback image digest does not match the frozen bundle: $service" >&2
        exit 2
    fi
}

verify_bundle_image agent-runtime "$rollback_runtime_image"
verify_bundle_image assistant-service "$rollback_assistant_image"
verify_bundle_image gateway "$rollback_gateway_image"
verify_bundle_image frontend "$rollback_frontend_image"
verify_bundle_image knowledge-service "$rollback_knowledge_image"
verify_bundle_image postgres "$rollback_postgres_image"
verify_bundle_image redis "$rollback_redis_image"
verify_bundle_image qdrant "$rollback_qdrant_image"

owner="$(docker inspect ai-gateway-backend --format '{{index .Config.Labels "com.docker.compose.project.working_dir"}}' 2>/dev/null || true)"
if [[ -n "$owner" && "$owner" != "$repo_root" ]]; then
    echo "ERROR: running stack belongs to another checkout: $owner" >&2
    exit 2
fi

temp_root="$(mktemp -d /tmp/ai-platform-agent-rollback-rehearsal.XXXXXX)"
old_compose="$temp_root/docker-compose.yml"
compat_override="$temp_root/current-knowledge-compat.yml"
restore_required=0

clean_env() {
    env -i HOME="$HOME" PATH="$PATH" TMPDIR="${TMPDIR:-/tmp}" \
        DOCKER_CONFIG="${DOCKER_CONFIG:-$HOME/.docker}" \
        COMPOSE_PARALLEL_LIMIT="${COMPOSE_PARALLEL_LIMIT:-1}" "$@"
}

old_compose_cmd() {
    clean_env AI_PLATFORM_AGENT_RUNTIME_IMAGE="$rollback_runtime_image" \
        AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION="$rollback_kernel_revision" \
        ASSISTANT_IMAGE="$rollback_assistant_image" \
        GATEWAY_IMAGE="$rollback_gateway_image" \
        FRONTEND_IMAGE="$rollback_frontend_image" \
        KNOWLEDGE_IMAGE="$rollback_knowledge_image" \
        POSTGRES_IMAGE="$rollback_postgres_image" \
        REDIS_IMAGE="$rollback_redis_image" \
        QDRANT_IMAGE="$rollback_qdrant_image" \
        docker compose --project-name "$project_name" --project-directory "$repo_root" \
        --env-file "$env_file" -f "$old_compose" "$@"
}

current_compose_cmd() {
    clean_env docker compose --project-name "$project_name" --project-directory "$repo_root" \
        --env-file "$env_file" -f "$repo_root/docker-compose.yml" "$@"
}

current_knowledge_compose_cmd() {
    clean_env docker compose --project-name "$project_name" --project-directory "$repo_root" \
        --env-file "$env_file" -f "$repo_root/docker-compose.yml" -f "$compat_override" "$@"
}

wait_healthy() {
    local deadline=$((SECONDS + 240))
    local container status
    for container in "$@"; do
        while (( SECONDS < deadline )); do
            status="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container" 2>/dev/null || true)"
            if [[ "$status" == "healthy" ]]; then
                break
            fi
            if [[ "$status" == "unhealthy" || "$status" == "exited" || "$status" == "dead" ]]; then
                echo "ERROR: $container entered terminal state $status" >&2
                return 1
            fi
            sleep 2
        done
        if (( SECONDS >= deadline )); then
            echo "ERROR: timed out waiting for $container" >&2
            return 1
        fi
    done
}

assert_quiescent() {
    local active
    active="$(docker exec ai-gateway-pg psql \
        -U "$postgres_user" -d "$postgres_db" -Atqc \
        "SELECT
            (SELECT count(*) FROM assistant.assistant_runs
             WHERE status IN ('running','pending','awaiting_approval')
               AND updated_at > NOW() - INTERVAL '15 minutes')
          + (SELECT count(*) FROM assistant.assistant_tool_approvals
             WHERE status = 'pending' AND expires_at > NOW())
          + (SELECT count(*) FROM gateway.assistant_capability_executions
             WHERE status NOT IN ('succeeded','failed','cancelled','timeout','side_effect_unknown'));")"
    if [[ "$active" != "0" ]]; then
        echo "ERROR: rollback rehearsal requires a quiescent stack; active work exists" >&2
        exit 2
    fi
}

db_fingerprint() {
    docker exec ai-gateway-pg psql \
        -U "$postgres_user" -d "$postgres_db" -Atqc \
        "SELECT json_build_object(
            'sessions',(SELECT count(*) FROM assistant.sessions),
            'sessions_digest',(SELECT md5(COALESCE(string_agg(md5(to_jsonb(s)::text),'' ORDER BY session_id),'')) FROM assistant.sessions s),
            'migrations',(SELECT count(*) FROM public.schema_migrations),
            'migrations_digest',(SELECT md5(COALESCE(string_agg(md5(to_jsonb(m)::text),'' ORDER BY filename),'')) FROM public.schema_migrations m),
            'schema_digest',(
                SELECT md5(COALESCE(string_agg(md5(to_jsonb(c)::text),'' ORDER BY table_schema,table_name,ordinal_position),''))
                FROM information_schema.columns c
                WHERE (table_schema,table_name) IN (
                    ('assistant','sessions'),
                    ('gateway','assistant_runtime_threads'),
                    ('gateway','assistant_runtime_items'),
                    ('gateway','assistant_capability_executions')
                )
            ),
            'threads',(SELECT count(*) FROM gateway.assistant_runtime_threads),
            'threads_digest',(SELECT md5(COALESCE(string_agg(md5(to_jsonb(t)::text),'' ORDER BY runtime_thread_id),'')) FROM gateway.assistant_runtime_threads t),
            'items',(SELECT count(*) FROM gateway.assistant_runtime_items),
            'items_digest',(SELECT md5(COALESCE(string_agg(md5(to_jsonb(i)::text),'' ORDER BY runtime_thread_id,sequence),'')) FROM gateway.assistant_runtime_items i),
            'capability_executions',(SELECT count(*) FROM gateway.assistant_capability_executions),
            'capability_executions_digest',(SELECT md5(COALESCE(string_agg(md5(to_jsonb(e)::text),'' ORDER BY execution_id),'')) FROM gateway.assistant_capability_executions e),
            'duplicate_execution_keys',(
                SELECT count(*) FROM (
                    SELECT run_id,tool_call_id,attempt_id
                    FROM gateway.assistant_capability_executions
                    GROUP BY 1,2,3 HAVING count(*) > 1
                ) duplicates
            )
        );"
}

api_fingerprint() {
    python3 - "$credential_file" <<'PY'
import hashlib
import json
import sys
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
credentials = json.loads(open(sys.argv[1], encoding="utf-8").read())
request = urllib.request.Request(
    "http://127.0.0.1:8080/api/v1/auth/login",
    data=json.dumps(credentials).encode(),
    headers={"Content-Type": "application/json"},
    method="POST",
)
with opener.open(request, timeout=30) as response:
    login = json.load(response)
token = login["access_token"]
headers = {"Authorization": f"Bearer {token}"}
request = urllib.request.Request(
    "http://127.0.0.1:8080/api/v1/assistant/sessions?limit=50", headers=headers
)
with opener.open(request, timeout=30) as response:
    payload = json.load(response)
sessions = payload if isinstance(payload, list) else payload.get("sessions", [])
session_ids = sorted((item.get("session_id") or item.get("id")) for item in sessions)
history_digests = []
history_count = 0
for session_id in session_ids:
    request = urllib.request.Request(
        f"http://127.0.0.1:8080/api/v1/assistant/sessions/{session_id}/history?limit=500",
        headers=headers,
    )
    with opener.open(request, timeout=30) as response:
        history_payload = json.load(response)
    history = (
        history_payload
        if isinstance(history_payload, list)
        else history_payload.get("history", history_payload.get("messages", []))
    )
    canonical_history = json.dumps(history, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    history_count += len(history)
    history_digests.append(f"{session_id}:{hashlib.sha256(canonical_history.encode()).hexdigest()}")

result = {
    "session_count": len(sessions),
    "session_set_sha256": hashlib.sha256("\n".join(session_ids).encode()).hexdigest(),
    "sample_session_sha256": None,
    "history_count": history_count,
    "history_sha256": hashlib.sha256("\n".join(history_digests).encode()).hexdigest(),
}
if sessions:
    session_id = sessions[0].get("session_id") or sessions[0].get("id")
    result["sample_session_sha256"] = hashlib.sha256(session_id.encode()).hexdigest()
print(json.dumps(result, sort_keys=True, separators=(",", ":")))
PY
}

restore_current() {
    current_knowledge_compose_cmd up -d --no-build --no-deps --force-recreate \
        knowledge-service knowledge-worker
    wait_healthy ai-gateway-knowledge-service ai-gateway-knowledge-worker
    current_compose_cmd up -d --no-build --no-deps --force-recreate \
        agent-capability-worker agent-runtime gateway frontend
    current_compose_cmd up -d --no-build --no-deps --remove-orphans agent-runtime
    make -C "$repo_root" ENV_FILE="$env_file" hot-update ARGS="--all"
    make -C "$repo_root" ENV_FILE="$env_file" migrate
    wait_healthy ai-gateway-pg ai-gateway-redis ai-gateway-qdrant \
        ai-gateway-knowledge-service ai-gateway-knowledge-worker \
        ai-gateway-agent-capability-worker ai-gateway-agent-runtime \
        ai-gateway-backend ai-gateway-frontend
}

cleanup() {
    local exit_code=$?
    local recovery_failed=0
    if (( restore_required == 1 )); then
        echo "Restoring current Runtime after interrupted rollback rehearsal..." >&2
        restore_current || recovery_failed=1
    fi
    find "$temp_root" -depth -delete 2>/dev/null || true
    if (( recovery_failed == 1 )); then
        echo "ERROR: rollback rehearsal failed and the current release could not be restored" >&2
        docker ps --filter 'name=ai-gateway-' --format '{{.Names}} {{.Status}}' >&2 || true
        trap - EXIT
        exit 70
    fi
    return "$exit_code"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

git archive "$rollback_commit" docker-compose.yml | tar -x -C "$temp_root"
python3 - "$compat_override" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_text(
    'services:\n'
    '  knowledge-service:\n'
    '    environment:\n'
    '      GATEWAY_ASSISTANT_SHARED_SECRET: "${GATEWAY_ASSISTANT_SHARED_SECRET}"\n'
    '  knowledge-worker:\n'
    '    environment:\n'
    '      GATEWAY_ASSISTANT_SHARED_SECRET: "${GATEWAY_ASSISTANT_SHARED_SECRET}"\n',
    encoding='utf-8',
)
PY
actual_compose_sha="$(shasum -a 256 "$old_compose" | awk '{print $1}')"
if [[ "$actual_compose_sha" != "$rollback_compose_sha" ]]; then
    echo "ERROR: rollback Compose does not match the frozen bundle" >&2
    exit 2
fi

resolved_compose_sha="$(old_compose_cmd config | shasum -a 256 | awk '{print $1}')"
if [[ "$resolved_compose_sha" != "$rollback_resolved_compose_sha" ]]; then
    echo "ERROR: resolved rollback Compose does not match the frozen bundle" >&2
    exit 2
fi

assert_quiescent
before_db="$(db_fingerprint)"
before_api="$(api_fingerprint)"
restore_required=1

old_compose_cmd up -d --no-build --no-deps --force-recreate \
    knowledge-service knowledge-worker
wait_healthy ai-gateway-knowledge-service ai-gateway-knowledge-worker
old_compose_cmd up -d --no-build --no-deps --force-recreate assistant-service agent-runtime
wait_healthy ai-gateway-assistant-service ai-gateway-agent-runtime
old_compose_cmd up -d --no-build --no-deps --force-recreate gateway frontend
wait_healthy ai-gateway-pg ai-gateway-redis ai-gateway-qdrant \
    ai-gateway-knowledge-service ai-gateway-knowledge-worker \
    ai-gateway-assistant-service ai-gateway-agent-runtime \
    ai-gateway-backend ai-gateway-frontend
old_db="$(db_fingerprint)"
old_api="$(api_fingerprint)"

if [[ "$before_db" != "$old_db" ]]; then
    echo "ERROR: frozen release changed the database fingerprint" >&2
    echo "before_db=$before_db" >&2
    echo "rollback_db=$old_db" >&2
    exit 1
fi
if [[ "$before_api" != "$old_api" ]]; then
    echo "ERROR: frozen release changed the session API fingerprint" >&2
    echo "before_api=$before_api" >&2
    echo "rollback_api=$old_api" >&2
    exit 1
fi

restore_current
restore_required=0
after_db="$(db_fingerprint)"
after_api="$(api_fingerprint)"

if [[ "$before_db" != "$after_db" || "$before_api" != "$after_api" ]]; then
    echo "ERROR: current release did not recover the database/API fingerprint" >&2
    exit 1
fi
if docker container inspect ai-gateway-assistant-service >/dev/null 2>&1; then
    echo "ERROR: legacy Assistant container remains after restoring the current release" >&2
    exit 1
fi

mkdir -p "$(dirname "$report_path")"
python3 - "$report_path" "$rollback_commit" "$rollback_runtime_image" \
    "$rollback_assistant_image" "$current_runtime_image" "$current_worker_image" \
    "$before_db" "$old_db" "$after_db" "$before_api" "$old_api" "$after_api" <<'PY'
import datetime
import json
import pathlib
import sys

(
    report_path,
    rollback_commit,
    rollback_runtime,
    rollback_assistant,
    current_runtime,
    current_worker,
    before_db,
    old_db,
    after_db,
    before_api,
    old_api,
    after_api,
) = sys.argv[1:]
payload = {
    "schema_version": "agent-runtime-rollback-rehearsal/v1",
    "completed_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "sequence": ["current_new", "frozen_old", "current_new"],
    "rollback_commit": rollback_commit,
    "images": {
        "rollback_runtime": rollback_runtime,
        "rollback_assistant": rollback_assistant,
        "current_runtime": current_runtime,
        "current_worker": current_worker,
    },
    "database": {
        "before": json.loads(before_db),
        "rollback": json.loads(old_db),
        "restored": json.loads(after_db),
        "preserved": before_db == old_db == after_db,
    },
    "api": {
        "before": json.loads(before_api),
        "rollback": json.loads(old_api),
        "restored": json.loads(after_api),
        "preserved": before_api == old_api == after_api,
    },
    "schema_downgrade": not (
        json.loads(before_db)["migrations_digest"]
        == json.loads(old_db)["migrations_digest"]
        == json.loads(after_db)["migrations_digest"]
    ),
    "volumes_deleted": False,
    "status": "pass",
}
path = pathlib.Path(report_path)
path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY

echo "agent-runtime-rollback-rehearsal: PASS"
echo "report=$report_path"
