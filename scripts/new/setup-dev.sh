#!/bin/bash
# =============================================================================
# AI Gateway - repository-scoped development infrastructure
# =============================================================================
# Usage: make dev-start / make dev-status or ./scripts/new/setup-dev.sh [OPTIONS]
# The existing local Compose project and its named volumes are reused. The
# development env defaults to .env.dev; an explicit --env path remains exact.
#
# Options:
#   (default)    Start infra, run native database authority, sync workspace packages
#   --start      Start the four infrastructure services only
#   --stop       Stop this repository's infrastructure services
#   --reset      Recreate containers while preserving Compose volumes and data
#   --status     Show this repository's infrastructure status
#   --env FILE   Use a specific env file
# =============================================================================

SETUP_SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_PROJECT_ROOT="$(cd "$SETUP_SCRIPT_DIR/../.." && pwd)"
# Makefile's generic default is .env. Local development for this source
# checkpoint is explicitly bound to the preserved .env.dev configuration.
if [[ -z "${ENV_FILE:-}" || "${ENV_FILE:-}" == ".env" ]] \
    && [[ -f "$SETUP_PROJECT_ROOT/.env.dev" ]]; then
    ENV_FILE="$SETUP_PROJECT_ROOT/.env.dev"
fi
source "$SETUP_SCRIPT_DIR/common.sh"

# -- Parse args --------------------------------------------------------------
ACTION="full"

while [[ $# -gt 0 ]]; do
    case $1 in
        --start)  ACTION="start"; shift ;;
        --stop)   ACTION="stop"; shift ;;
        --reset)  ACTION="reset"; shift ;;
        --status) ACTION="status"; shift ;;
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            ENV_FILE="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [--start|--stop|--reset|--status] [--env FILE]"
            exit 0 ;;
        *) log_error "Unknown: $1"; exit 1 ;;
    esac
done

require_env_file
# Development Compose and migration credentials come from the selected env
# file alone. Do not let ambient values silently retarget this repository's
# ports or authority DSNs to another stack.
unset \
    COMPOSE_PROJECT_NAME \
    POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB POSTGRES_PORT POSTGRES_HOST \
    REDIS_PASSWORD REDIS_PORT QDRANT_HTTP_PORT QDRANT_GRPC_PORT \
    LOCAL_S3_ACCESS_KEY LOCAL_S3_SECRET_KEY LOCAL_S3_PORT LOCAL_S3_CONSOLE_PORT \
    LOCAL_DB_MIGRATOR_PASSWORD AI_GATEWAY_ROLE_PREFIX \
    AI_GATEWAY_DATABASE_ADMIN_DSN AI_GATEWAY_DATABASE_MIGRATOR_DSN
load_env

# This is the already-running repository-owned Compose project. Keep the
# project identity and compose source fixed so dev commands cannot create a
# parallel project based on legacy container names or root deployment files.
DEV_COMPOSE_PROJECT="${COMPOSE_PROJECT_NAME:-agentforge-dev-$(printf %s "$PROJECT_ROOT" | shasum -a 256 | cut -c1-10)}"
if [[ ! "$DEV_COMPOSE_PROJECT" =~ ^[a-z0-9][a-z0-9_-]*$ ]]; then
    log_error "COMPOSE_PROJECT_NAME must be a valid lowercase Compose project name"
    exit 2
fi
DEV_COMPOSE_FILE="$PROJECT_ROOT/deploy/local/compose.yml"
DEV_SERVICES=(postgres redis qdrant object-store)
COMPOSE_CMD=""

PG_USER="${POSTGRES_USER:-postgres}"
PG_HOST="${POSTGRES_HOST:-127.0.0.1}"
PG_PORT="${POSTGRES_PORT:-5432}"
PG_DB="${POSTGRES_DB:-gateway}"
REDIS_PASS="${REDIS_PASSWORD:-}"
REDIS_DEV_PORT="${REDIS_PORT:-6379}"
QDRANT_DEV_PORT="${QDRANT_HTTP_PORT:-6333}"
QDRANT_GRPC_DEV_PORT="${QDRANT_GRPC_PORT:-6334}"
S3_DEV_PORT="${LOCAL_S3_PORT:-9000}"
S3_CONSOLE_DEV_PORT="${LOCAL_S3_CONSOLE_PORT:-9001}"

compose_dev() {
    # get_compose_cmd returns the supported two-word `docker compose` command
    # or the legacy one-word `docker-compose` command.
    # shellcheck disable=SC2086
    $COMPOSE_CMD \
        --project-name "$DEV_COMPOSE_PROJECT" \
        --project-directory "$PROJECT_ROOT" \
        --file "$DEV_COMPOSE_FILE" \
        --env-file "$ENV_FILE" \
        "$@"
}

verify_dev_project_owner() {
    local container project owner config_files containers
    containers="$(docker ps -aq --filter "label=com.docker.compose.project=$DEV_COMPOSE_PROJECT")"
    while IFS= read -r container; do
        [ -n "$container" ] || continue
        project="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$container" 2>/dev/null || true)"
        owner="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$container" 2>/dev/null || true)"
        config_files="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.config_files" }}' "$container" 2>/dev/null || true)"
        if [ "$project" != "$DEV_COMPOSE_PROJECT" ] || [ "$owner" != "$PROJECT_ROOT" ]; then
            log_error "Refusing to manage a container outside this repository's Compose project"
            return 1
        fi
        case ",$config_files," in
            *,"$DEV_COMPOSE_FILE",*) ;;
            *)
                log_error "Refusing to manage a project container created from another Compose config"
                return 1
                ;;
        esac
    done <<< "$containers"
}

require_dev_runtime_config() {
    local key value missing=false
    for key in \
        POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB POSTGRES_PORT \
        REDIS_PASSWORD REDIS_PORT \
        QDRANT_HTTP_PORT QDRANT_GRPC_PORT \
        LOCAL_S3_ACCESS_KEY LOCAL_S3_SECRET_KEY LOCAL_S3_PORT LOCAL_S3_CONSOLE_PORT; do
        value="$(printenv "$key" 2>/dev/null || true)"
        if [ -z "$value" ]; then
            log_error "$key is required by deploy/local/compose.yml"
            missing=true
        fi
    done
    [ "$missing" = false ] || return 1
}

uri_component() {
    local LC_ALL=C value="$1" output="" character hex index
    for ((index = 0; index < ${#value}; index++)); do
        character="${value:index:1}"
        case "$character" in
            [a-zA-Z0-9.~_-]) output+="$character" ;;
            *) printf -v hex '%%%02X' "'${character}"; output+="$hex" ;;
        esac
    done
    printf '%s' "$output"
}

require_local_authority_config() {
    if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
        log_error "AI_GATEWAY_DATABASE_ADMIN_DSN is required for explicit local role provisioning"
        return 1
    fi
    if [ -z "${AI_GATEWAY_DATABASE_MIGRATOR_DSN:-}" ]; then
        if [ -z "${LOCAL_DB_MIGRATOR_PASSWORD:-}" ]; then
            log_error "Set AI_GATEWAY_DATABASE_MIGRATOR_DSN or LOCAL_DB_MIGRATOR_PASSWORD"
            return 1
        fi
        local prefix="${AI_GATEWAY_ROLE_PREFIX:-ai_gateway_}"
        local url_host encoded_user encoded_password encoded_database
        if [[ ! "$prefix" =~ ^[a-z][a-z0-9_]{0,20}_$ ]]; then
            log_error "AI_GATEWAY_ROLE_PREFIX is invalid"
            return 1
        fi
        if [[ "$PG_HOST" =~ : ]]; then
            if [[ ! "$PG_HOST" =~ ^[0-9A-Fa-f:.]+$ ]]; then
                log_error "POSTGRES_HOST is invalid for a local authority DSN"
                return 1
            fi
            url_host="[$PG_HOST]"
        else
            if [[ ! "$PG_HOST" =~ ^[A-Za-z0-9.-]+$ ]]; then
                log_error "POSTGRES_HOST is invalid for a local authority DSN"
                return 1
            fi
            url_host="$PG_HOST"
        fi
        if [[ ! "$PG_PORT" =~ ^[0-9]{1,5}$ ]] \
            || (( 10#$PG_PORT < 1 || 10#$PG_PORT > 65535 )); then
            log_error "POSTGRES_PORT is invalid"
            return 1
        fi
        encoded_user="$(uri_component "${prefix}migrator")"
        encoded_password="$(uri_component "$LOCAL_DB_MIGRATOR_PASSWORD")"
        encoded_database="$(uri_component "$PG_DB")"
        AI_GATEWAY_DATABASE_MIGRATOR_DSN="postgresql://${encoded_user}:${encoded_password}@${url_host}:${PG_PORT}/${encoded_database}"
        export AI_GATEWAY_DATABASE_MIGRATOR_DSN
    fi
}

start_services() {
    require_dev_runtime_config || return 1
    verify_dev_project_owner || return 1
    log_step "Starting the existing repository infrastructure project"
    compose_dev up -d --no-recreate --pull never "${DEV_SERVICES[@]}"
}

stop_services() {
    verify_dev_project_owner || return 1
    compose_dev stop "${DEV_SERVICES[@]}"
}

recreate_services_preserving_data() {
    require_dev_runtime_config || return 1
    verify_dev_project_owner || return 1
    log_step "Recreating containers without removing project volumes"
    compose_dev up -d --force-recreate --pull never "${DEV_SERVICES[@]}"
}

postgres_service_ready() {
    compose_dev exec -T postgres pg_isready -U "$PG_USER" -d "$PG_DB" >/dev/null 2>&1
}

redis_service_ready() {
    compose_dev exec -T -e "REDISCLI_AUTH=$REDIS_PASS" redis redis-cli ping 2>/dev/null \
        | grep -q PONG
}

qdrant_service_ready() {
    curl -sf "http://127.0.0.1:${QDRANT_DEV_PORT}/healthz" >/dev/null
}

object_store_service_ready() {
    compose_dev exec -T object-store curl -fsS http://localhost:9000/health >/dev/null 2>&1
}

wait_all_healthy() {
    wait_for_healthy "PostgreSQL" "postgres_service_ready" 60
    wait_for_healthy "Redis" "redis_service_ready" 60
    wait_for_healthy "Qdrant" "qdrant_service_ready" 60
    wait_for_healthy "Object Store" "object_store_service_ready" 60
}

init_db() {
    require_local_authority_config || return 1
    log_step "Native database authority"
    log_info "Using the configured admin DSN only for role provisioning and the distinct migrator DSN for migrations."
    ENV_FILE="$ENV_FILE" "$SETUP_SCRIPT_DIR/migrate.sh" --auto
}

show_status() {
    echo ""
    echo "Repository Compose project: $DEV_COMPOSE_PROJECT"
    echo "Compose config: $DEV_COMPOSE_FILE"
    echo "Infrastructure status:"
    verify_dev_project_owner || return 1
    compose_dev ps --all

    echo ""
    echo "Connection Info:"
    echo "  PostgreSQL: postgresql://${PG_USER}:****@${PG_HOST}:${PG_PORT}/${PG_DB}"
    echo "  Redis:      redis://:****@127.0.0.1:${REDIS_DEV_PORT}"
    echo "  Qdrant:     http://127.0.0.1:${QDRANT_DEV_PORT}"
    echo "  Object:     http://127.0.0.1:${S3_DEV_PORT}"
    echo "  S3 console: http://127.0.0.1:${S3_CONSOLE_DEV_PORT}"
    echo ""
}

# -- Execute action ----------------------------------------------------------
require_docker
COMPOSE_CMD="$(get_compose_cmd)"
cd "$PROJECT_ROOT"

case $ACTION in
    stop)
        stop_services
        log_success "Repository infrastructure services stopped"
        ;;
    status)
        show_status
        ;;
    start)
        start_services
        wait_all_healthy
        show_status
        ;;
    reset)
        recreate_services_preserving_data
        wait_all_healthy
        init_db
        sync_workspace_packages
        show_status
        log_success "Development containers recreated; project volumes and data were preserved"
        ;;
    full)
        start_services
        wait_all_healthy
        init_db
        sync_workspace_packages
        show_status
        log_success "Dev environment ready!"
        echo "Next steps:"
        echo "  1. Backend:  gateway --config-env"
        echo "  2. Frontend: pnpm -C web dev"
        echo ""
        ;;
esac
