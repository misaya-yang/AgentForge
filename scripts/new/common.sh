#!/bin/bash
# =============================================================================
# AI Gateway - Shared Functions
# =============================================================================
# Source this file in other scripts: source "$(dirname "$0")/common.sh"
# =============================================================================

set -euo pipefail

# -- Project paths -----------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_ENV_FILE="${PROJECT_ROOT}/.env"
ENV_FILE="${ENV_FILE:-$DEFAULT_ENV_FILE}"

# -- Colors ------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# -- Logging -----------------------------------------------------------------
log_info()    { echo -e "${BLUE}[INFO]${NC}    $1"; }
log_success() { echo -e "${GREEN}[OK]${NC}      $1"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC}    $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC}   $1"; }
log_step()    { echo -e "\n${CYAN}${BOLD}=> $1${NC}"; }

# -- Environment loading -----------------------------------------------------
env_file_path() {
    echo "${ENV_FILE:-$DEFAULT_ENV_FILE}"
}

load_env() {
    local env_file
    env_file="$(env_file_path)"
    if [ -f "$env_file" ]; then
        set -a
        # Parse KEY=VALUE lines only (skip comments, empty lines; no arbitrary execution)
        while IFS='=' read -r key value; do
            # Skip comments and empty lines
            [[ "$key" =~ ^[[:space:]]*# ]] && continue
            [[ -z "$key" ]] && continue
            # Strip leading/trailing whitespace from key
            key=$(echo "$key" | xargs)
            # Only export valid variable names
            if [[ "$key" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]]; then
                value="${value%$'\r'}"
                if [[ "$value" =~ ^\".*\"$ ]] || [[ "$value" =~ ^\'.*\'$ ]]; then
                    value="${value:1:${#value}-2}"
                fi
                # Callers may pin a release/runtime image explicitly. Keep
                # those exported values authoritative; the env file supplies
                # defaults only for variables that are not already present.
                if [[ ${!key+x} ]]; then
                    continue
                fi
                export "$key=$value"
            fi
        done < "$env_file"
        set +a
    fi
}

# -- Docker compose command detection ----------------------------------------
get_compose_cmd() {
    if docker compose version &>/dev/null; then
        echo "docker compose"
    elif command -v docker-compose &>/dev/null; then
        echo "docker-compose"
    else
        log_error "Docker Compose not found"
        exit 1
    fi
}

# -- Requirement checks ------------------------------------------------------
require_docker() {
    if ! command -v docker &>/dev/null; then
        log_error "Docker is not installed"
        exit 1
    fi
    if ! docker info &>/dev/null; then
        log_error "Docker daemon is not running"
        exit 1
    fi
}

require_env_file() {
    local env_file
    env_file="$(env_file_path)"
    if [ ! -f "$env_file" ]; then
        log_error "Env file not found: $env_file. Copy .env.example to .env or set ENV_FILE to a populated env file."
        exit 1
    fi
}

# -- Database config from environment ----------------------------------------
# Reads PG config from env vars (loaded via load_env)
pg_host()     { echo "${POSTGRES_HOST:-localhost}"; }
pg_port()     { echo "${POSTGRES_PORT:-5432}"; }
pg_user()     { echo "${POSTGRES_USER:-postgres}"; }
pg_password() { echo "${POSTGRES_PASSWORD:?POSTGRES_PASSWORD is required}"; }
pg_database() { echo "${POSTGRES_DB:-gateway}"; }

# Container names (overridable via env vars for dev setup)
compose_project_name() { echo "${COMPOSE_PROJECT_NAME:-ai-gateway}"; }
compose_service_containers() {
    local service="$1"
    docker ps -a \
        --filter "label=com.docker.compose.project=$(compose_project_name)" \
        --filter "label=com.docker.compose.service=$service" \
        --format '{{.Names}}' 2>/dev/null
}
compose_running_service_containers() {
    local service="$1"
    docker ps \
        --filter "label=com.docker.compose.project=$(compose_project_name)" \
        --filter "label=com.docker.compose.service=$service" \
        --format '{{.Names}}' 2>/dev/null
}
compose_service_container() {
    local service="$1"
    local fallback="$2"
    local discovered
    discovered="$(compose_service_containers "$service" | head -n 1)"
    echo "${discovered:-$fallback}"
}
topology_service_present() {
    local service="$1"
    [ "$(topology_service_replicas "$service")" -gt 0 ]
}
topology_service_replicas() {
    local service="$1"
    local mode="${AI_PLATFORM_TOPOLOGY_MODE:-full}"
    python3 "$PROJECT_ROOT/scripts/deploy/topology_modes.py" \
        --mode "$mode" --service-replicas "$service"
}
topology_service_ids() {
    local scope="$1"
    local mode="${AI_PLATFORM_TOPOLOGY_MODE:-full}"
    python3 "$PROJECT_ROOT/scripts/deploy/topology_modes.py" \
        --mode "$mode" --service-ids "$scope"
}
running_service_replica_count() {
    local service="$1"
    compose_running_service_containers "$service" | awk 'NF { count += 1 } END { print count + 0 }'
}
check_topology_cardinality() {
    local service expected actual
    for service in gateway agent-runtime knowledge-worker agent-capability-worker; do
        expected="$(topology_service_replicas "$service")"
        actual="$(running_service_replica_count "$service")"
        if [ "$actual" -ne "$expected" ]; then
            log_error "$service running replicas $actual do not match topology $expected"
            return 1
        fi
    done
}
pg_container()     { echo "${POSTGRES_CONTAINER:-ai-gateway-pg}"; }
redis_container()  { echo "${REDIS_CONTAINER:-ai-gateway-redis}"; }
qdrant_container() { echo "${QDRANT_CONTAINER:-ai-gateway-qdrant}"; }
knowledge_container() { echo "${KNOWLEDGE_CONTAINER:-ai-gateway-knowledge-service}"; }
knowledge_worker_container() {
    compose_service_container "knowledge-worker" "${KNOWLEDGE_WORKER_CONTAINER:-ai-gateway-knowledge-worker}"
}
gateway_container()   { echo "${GATEWAY_CONTAINER:-ai-gateway-backend}"; }
frontend_container()  { echo "${FRONTEND_CONTAINER:-ai-gateway-frontend}"; }
agent_runtime_container() { echo "${AGENT_RUNTIME_CONTAINER:-ai-gateway-agent-runtime}"; }
agent_capability_worker_container() {
    compose_service_container "agent-capability-worker" \
        "${AGENT_CAPABILITY_WORKER_CONTAINER:-ai-gateway-agent-capability-worker}"
}

agent_runtime_kernel_revision() {
    python3 - "$PROJECT_ROOT/deploy/agent-runtime-source/source-receipt.json" <<'PY'
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
source = receipt["source"]["upstream_sha"]
overlay = receipt["overlay"]["sha256"]
print(f"{source}+{overlay[:12]}")
PY
}

agent_runtime_image_tag() {
    python3 - "$PROJECT_ROOT/deploy/agent-runtime-source/source-receipt.json" <<'PY'
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
upstream = receipt["source"]["upstream_sha"]
overlay = receipt["overlay"]["sha256"]
print(f"ai-gateway-agent-runtime:local-{upstream[:12]}-{overlay[:12]}")
PY
}

agent_capability_worker_image_tag() {
    python3 - "$PROJECT_ROOT/deploy/agent-runtime-source/source-receipt.json" <<'PY'
import json
import sys

receipt = json.load(open(sys.argv[1], encoding="utf-8"))
upstream = receipt["source"]["upstream_sha"]
overlay = receipt["overlay"]["sha256"]
print(f"ai-gateway-agent-capability-worker:local-{upstream[:12]}-{overlay[:12]}")
PY
}

assert_agent_runtime_image_locked() {
    local task_image="$1"
    if ! docker image inspect "$task_image" >/dev/null 2>&1; then
        log_error "Configured Agent Runtime image is not available locally: $task_image"
        return 1
    fi
    if ! python3 "$PROJECT_ROOT/scripts/harness/agent_runtime_supply_chain.py" validate \
        --repo-root "$PROJECT_ROOT" \
        --lock "$PROJECT_ROOT/deploy/agent-runtime-source/lock.json" \
        --require-artifact agent_runtime >/dev/null; then
        log_error "Agent Runtime source/image lock validation failed"
        return 1
    fi

    local task_locked_digest
    local task_image_digest
    task_locked_digest="$(python3 - "$PROJECT_ROOT/deploy/agent-runtime-source/lock.json" <<'PY'
import json
import sys

lock = json.load(open(sys.argv[1], encoding="utf-8"))
print(lock["oci"]["artifacts"]["agent_runtime"]["image_digest"] or "")
PY
)"
    task_image_digest="$(docker image inspect "$task_image" --format '{{.Id}}')"
    if [ -z "$task_locked_digest" ] || [ "$task_image_digest" != "$task_locked_digest" ]; then
        log_error "Configured Agent Runtime image digest does not match the locked artifact"
        return 1
    fi
    return 0
}

assert_runtime_release_unit_locked() {
    local task_runtime_image="$1"
    local task_worker_image="$2"
    if ! python3 "$PROJECT_ROOT/scripts/harness/agent_runtime_supply_chain.py" verify-local-images \
        --repo-root "$PROJECT_ROOT" \
        --lock "$PROJECT_ROOT/deploy/agent-runtime-source/lock.json" \
        --runtime-image "$task_runtime_image" \
        --worker-image "$task_worker_image" >/dev/null; then
        log_error "Runtime/capability worker image, platform, or source identity does not match the locked release unit"
        return 1
    fi
}

# -- Compose ownership guard -------------------------------------------------
assert_compose_owner() {
    local expected_owner="${1:-$PROJECT_ROOT}"
    local inspected_names=(
        "$(pg_container)"
        "$(redis_container)"
        "$(qdrant_container)"
        "$(gateway_container)"
        "$(frontend_container)"
        "$(knowledge_container)"
        "$(agent_runtime_container)"
        # Legacy/other-checkout names are intentionally not part of this
        # repository's runtime ownership check.
        ai-gateway-knowledge
        islamic-content-service
    )
    local discovered
    for discovered in \
        $(compose_service_containers knowledge-worker) \
        $(compose_service_containers agent-capability-worker); do
        inspected_names+=("$discovered")
    done
    local container owner project service mismatch=false

    for container in "${inspected_names[@]}"; do
        if ! docker inspect "$container" >/dev/null 2>&1; then
            continue
        fi
        owner=$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$container" 2>/dev/null || true)
        project=$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$container" 2>/dev/null || true)
        service=$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.service" }}' "$container" 2>/dev/null || true)
        if [ -z "$owner" ]; then
            log_error "Container '$container' (service=${service:-unknown}) has no Compose working-directory label"
            mismatch=true
        elif [ "$owner" != "$expected_owner" ]; then
            log_error "Container '$container' (service=${service:-unknown}) belongs to a different checkout: $owner"
            mismatch=true
        elif [ "$project" != "ai-gateway" ]; then
            log_error "Container '$container' (service=${service:-unknown}) belongs to a different Compose project: ${project:-unlabeled}"
            mismatch=true
        fi
    done

    if [ "$mismatch" = true ]; then
        log_error "Refusing to mutate Docker project 'ai-gateway' from $PROJECT_ROOT until wrong-checkout containers are stopped or removed explicitly."
        exit 1
    fi
}

# -- SQL execution helpers ---------------------------------------------------
# Run SQL via docker exec (production) or psql (dev)
run_sql() {
    local sql="$1"
    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        docker exec -i "$(pg_container)" psql -U "$(pg_user)" -d "$(pg_database)" -c "$sql" 2>/dev/null
    elif command -v psql &>/dev/null; then
        PGPASSWORD="$(pg_password)" psql -h "$(pg_host)" -p "$(pg_port)" -U "$(pg_user)" -d "$(pg_database)" -c "$sql" 2>/dev/null
    else
        log_error "Cannot connect to PostgreSQL (no docker container or psql found)"
        return 1
    fi
}

run_sql_file() {
    local file="$1"
    local search_path="${2:-}"
    local search_path_sql=""
    if [ -n "$search_path" ]; then
        case "$search_path" in
            public|knowledge,gateway,assistant,public) ;;
            *)
                log_error "Unsupported migration search_path: $search_path"
                return 1
                ;;
        esac
        search_path_sql="SET search_path TO ${search_path};"
    fi
    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        if [ -n "$search_path_sql" ]; then
            { printf '%s\n' "$search_path_sql"; cat "$file"; } \
                | docker exec -i "$(pg_container)" psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$(pg_database)" 2>&1
        else
            docker exec -i "$(pg_container)" psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$(pg_database)" < "$file" 2>&1
        fi
    elif command -v psql &>/dev/null; then
        if [ -n "$search_path_sql" ]; then
            { printf '%s\n' "$search_path_sql"; cat "$file"; } \
                | PGPASSWORD="$(pg_password)" psql -v ON_ERROR_STOP=1 -h "$(pg_host)" -p "$(pg_port)" -U "$(pg_user)" -d "$(pg_database)" 2>&1
        else
            PGPASSWORD="$(pg_password)" psql -v ON_ERROR_STOP=1 -h "$(pg_host)" -p "$(pg_port)" -U "$(pg_user)" -d "$(pg_database)" < "$file" 2>&1
        fi
    else
        log_error "Cannot connect to PostgreSQL"
        return 1
    fi
}

# -- Canonical migration serialization --------------------------------------
# Shared with database/cli.py. The helper keeps one dedicated PostgreSQL
# session alive for the whole shell migration plan; all DDL sessions cooperate
# by waiting on the same advisory lock. PostgreSQL releases the lock when this
# session disconnects, including process/connection crashes.
MIGRATION_ADVISORY_LOCK_NAMESPACE=1095781959
MIGRATION_ADVISORY_LOCK_ID=1
MIGRATION_LOCK_PID=""
MIGRATION_LOCK_TOKEN=""
MIGRATION_LOCK_DIR=""
MIGRATION_LOCK_FIFO=""
MIGRATION_LOCK_ACQUIRED=false

acquire_migration_advisory_lock() {
    MIGRATION_LOCK_TOKEN="ai_gateway_migrate_${PPID}_$$_${RANDOM}"
    MIGRATION_LOCK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ai-gateway-migration-lock.XXXXXX")"
    MIGRATION_LOCK_FIFO="${MIGRATION_LOCK_DIR}/stdin.fifo"
    mkfifo "$MIGRATION_LOCK_FIFO"

    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        docker exec -i -e "PGAPPNAME=${MIGRATION_LOCK_TOKEN}" "$(pg_container)" \
            psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$(pg_database)" \
            < "$MIGRATION_LOCK_FIFO" >/dev/null 2>&1 &
    elif command -v psql &>/dev/null; then
        PGAPPNAME="$MIGRATION_LOCK_TOKEN" PGPASSWORD="$(pg_password)" \
            psql -v ON_ERROR_STOP=1 -h "$(pg_host)" -p "$(pg_port)" \
            -U "$(pg_user)" -d "$(pg_database)" \
            < "$MIGRATION_LOCK_FIFO" >/dev/null 2>&1 &
    else
        rm -f "$MIGRATION_LOCK_FIFO"
        rmdir "$MIGRATION_LOCK_DIR"
        MIGRATION_LOCK_DIR=""
        MIGRATION_LOCK_FIFO=""
        log_error "Cannot connect to PostgreSQL to acquire migration lock"
        return 1
    fi
    MIGRATION_LOCK_PID=$!
    # FD 9 belongs to this runner. Normal exit writes unlock + \q; a hard
    # process crash closes the FD, psql sees EOF, and PostgreSQL releases the
    # session lock without any backend-wide cleanup query.
    exec 9>"$MIGRATION_LOCK_FIFO"
    printf 'SELECT pg_advisory_lock(%s, %s);\n' \
        "$MIGRATION_ADVISORY_LOCK_NAMESPACE" "$MIGRATION_ADVISORY_LOCK_ID" >&9

    while true; do
        if ! kill -0 "$MIGRATION_LOCK_PID" 2>/dev/null; then
            wait "$MIGRATION_LOCK_PID" 2>/dev/null || true
            MIGRATION_LOCK_PID=""
            exec 9>&-
            rm -f "$MIGRATION_LOCK_FIFO"
            rmdir "$MIGRATION_LOCK_DIR"
            MIGRATION_LOCK_DIR=""
            MIGRATION_LOCK_FIFO=""
            log_error "Migration lock session exited before acquiring the lock"
            return 1
        fi
        if run_sql "
            SELECT 1
            FROM pg_locks AS l
            JOIN pg_stat_activity AS a ON a.pid = l.pid
            WHERE a.application_name = '${MIGRATION_LOCK_TOKEN}'
              AND l.locktype = 'advisory'
              AND l.granted
              AND l.classid::bigint = ${MIGRATION_ADVISORY_LOCK_NAMESPACE}
              AND l.objid::bigint = ${MIGRATION_ADVISORY_LOCK_ID}
              AND l.objsubid = 2
            LIMIT 1;
        " 2>/dev/null | grep -q "1"; then
            MIGRATION_LOCK_ACQUIRED=true
            log_info "Acquired canonical migration lock"
            return 0
        fi
        sleep 0.2
    done
}

release_migration_advisory_lock() {
    if [ "$MIGRATION_LOCK_ACQUIRED" = true ] && [ -n "$MIGRATION_LOCK_PID" ]; then
        printf 'SELECT pg_advisory_unlock(%s, %s);\n\\q\n' \
            "$MIGRATION_ADVISORY_LOCK_NAMESPACE" "$MIGRATION_ADVISORY_LOCK_ID" \
            >&9 2>/dev/null || true
    fi
    exec 9>&- 2>/dev/null || true
    if [ -n "$MIGRATION_LOCK_PID" ]; then
        if [ "$MIGRATION_LOCK_ACQUIRED" != true ]; then
            kill "$MIGRATION_LOCK_PID" >/dev/null 2>&1 || true
        fi
        wait "$MIGRATION_LOCK_PID" 2>/dev/null || true
    fi
    if [ -n "$MIGRATION_LOCK_FIFO" ]; then
        rm -f "$MIGRATION_LOCK_FIFO"
    fi
    if [ -n "$MIGRATION_LOCK_DIR" ]; then
        rmdir "$MIGRATION_LOCK_DIR" 2>/dev/null || true
    fi
    MIGRATION_LOCK_PID=""
    MIGRATION_LOCK_TOKEN=""
    MIGRATION_LOCK_DIR=""
    MIGRATION_LOCK_FIFO=""
    MIGRATION_LOCK_ACQUIRED=false
}

# -- Health check helpers ----------------------------------------------------
wait_for_healthy() {
    local name="$1"
    local check_cmd="$2"
    local max_attempts="${3:-30}"
    local attempt=1

    log_info "Waiting for ${name}..."
    while [ $attempt -le "$max_attempts" ]; do
        if eval "$check_cmd" &>/dev/null; then
            log_success "${name} is ready"
            return 0
        fi
        printf "."
        sleep 1
        attempt=$((attempt + 1))
    done
    echo ""
    log_error "${name} not ready after ${max_attempts}s"
    return 1
}

check_postgres_health() {
    docker exec -e "PGPASSWORD=$(pg_password)" "$(pg_container)" \
        psql -h 127.0.0.1 -U "$(pg_user)" -d "$(pg_database)" -tAc "SELECT 1" 2>/dev/null \
        | grep -q 1
}

check_redis_health() {
    docker exec "$(redis_container)" sh -c \
        'REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli ping' 2>/dev/null | grep -q PONG
}

check_qdrant_health() {
    curl -sf "http://localhost:${QDRANT_HTTP_PORT:-6333}/healthz" &>/dev/null
}

check_gateway_health() {
    curl -sf "http://localhost:${GATEWAY_PORT:-8080}/health/ready" &>/dev/null
}

check_gateway_metrics() {
    local body
    local status
    local url="http://localhost:${GATEWAY_PORT:-8080}/metrics"

    body="$(curl -fsS "$url" 2>/dev/null)" && {
        printf '%s\n' "$body" | grep -Eq '^# HELP |^# TYPE ' || return 1
        printf '%s\n' "$body" | grep -Eq '^gateway_up($|[ {])'
        return
    }

    # Hardened deployments require auth for the Prometheus scrape endpoint.
    # Treat 401/403 as reachable-but-protected; connection errors and 5xx fail.
    status="$(curl -sS -o /dev/null -w "%{http_code}" "$url" 2>/dev/null || true)"
    if [ "$status" = "401" ] || [ "$status" = "403" ]; then
        return 0
    fi

    return 1
}

check_frontend_health() {
    curl -sf "http://localhost:${FRONTEND_PORT:-8081}/health" &>/dev/null
}

check_knowledge_health() {
    docker exec "$(knowledge_container)" curl -sf "http://127.0.0.1:8092/health/ready" &>/dev/null
}

check_knowledge_worker_health() {
    local containers container expected actual
    expected="$(topology_service_replicas knowledge-worker)"
    containers="$(compose_running_service_containers knowledge-worker)"
    actual="$(printf '%s\n' "$containers" | awk 'NF { count += 1 } END { print count + 0 }')"
    [ "$expected" -gt 0 ] && [ "$actual" -eq "$expected" ] || return 1
    while IFS= read -r container; do
        docker exec "$container" curl -sf "http://127.0.0.1:8092/health/ready" &>/dev/null \
            || return 1
    done <<< "$containers"
}

check_agent_runtime_health() {
    docker exec "$(agent_runtime_container)" curl -sf "http://127.0.0.1:8094/health/ready" &>/dev/null
}

check_agent_capability_worker_health() {
    local containers container expected actual
    expected="$(topology_service_replicas agent-capability-worker)"
    containers="$(compose_running_service_containers agent-capability-worker)"
    actual="$(printf '%s\n' "$containers" | awk 'NF { count += 1 } END { print count + 0 }')"
    [ "$expected" -gt 0 ] && [ "$actual" -eq "$expected" ] || return 1
    while IFS= read -r container; do
        docker exec "$container" ai-platform-capability-worker-health &>/dev/null || return 1
    done <<< "$containers"
}

# -- Python workspace packages -----------------------------------------------
sync_workspace_packages() {
    log_step "Syncing workspace Python packages"

    if command -v uv >/dev/null 2>&1; then
        (
            cd "$PROJECT_ROOT"
            uv sync --quiet
        ) && log_success "Workspace packages synced via uv" && return 0
    fi

    local pip_cmd=""
    if command -v pip >/dev/null 2>&1; then
        pip_cmd="pip"
    elif command -v pip3 >/dev/null 2>&1; then
        pip_cmd="pip3"
    fi

    if [ -n "$pip_cmd" ]; then
        (
            cd "$PROJECT_ROOT"
            "$pip_cmd" install -e packages/ai-gateway-core -q
        ) && log_success "Workspace packages synced via editable pip installs" && return 0
    fi

    log_warn "uv/pip not found; skipped workspace package sync"
    return 0
}

# -- Confirmation prompt -----------------------------------------------------
confirm() {
    local msg="${1:-Continue?}"
    if [[ "${AUTO_CONFIRM:-}" == "true" ]]; then
        return 0
    fi
    read -p "$msg [y/N] " -n 1 -r
    echo
    [[ $REPLY =~ ^[Yy]$ ]]
}
