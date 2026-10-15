#!/bin/bash
# =============================================================================
# AI Gateway - Local Deployment Hot Update
# =============================================================================
# Refresh source code inside existing local deployment containers without
# rebuilding images or running pip. Use this for Python/source-only changes.
#
# Usage:
#   make hot-update
#   make hot-update ARGS="--gateway"
#   make hot-update ARGS="--frontend"
#
# For dependency, Dockerfile, base image, or lockfile changes, use a rebuild.
# For continuous edit/reload, use: make dev-compose
# =============================================================================

source "$(dirname "$0")/common.sh"

UPDATE_GATEWAY=false
UPDATE_KNOWLEDGE=false
UPDATE_FRONTEND=false
NO_RESTART=false
EXPLICIT_SERVICE=false
EXPLICIT_ENV_FILE=false

usage() {
    cat <<'EOF'
Usage: scripts/new/hot-update.sh [OPTIONS]

Options:
  --gateway       Copy gateway src/config/database and shared packages, restart gateway
  --knowledge     Copy knowledge-service and shared packages, restart API and worker
  --frontend      Build web/dist locally and copy it into the nginx container
  --python        Update all Python services (gateway and knowledge)
  --all           Update all supported services, including frontend
  --no-restart    Copy files only
  --env FILE      Use a specific env file instead of .env
  -h, --help      Show this help

Default with no service flag: --python
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --gateway) UPDATE_GATEWAY=true; EXPLICIT_SERVICE=true; shift ;;
        --knowledge) UPDATE_KNOWLEDGE=true; EXPLICIT_SERVICE=true; shift ;;
        --frontend) UPDATE_FRONTEND=true; EXPLICIT_SERVICE=true; shift ;;
        --python)
            UPDATE_GATEWAY=true
            UPDATE_KNOWLEDGE=true
            EXPLICIT_SERVICE=true
            shift
            ;;
        --all)
            UPDATE_GATEWAY=true
            UPDATE_KNOWLEDGE=true
            UPDATE_FRONTEND=true
            EXPLICIT_SERVICE=true
            shift
            ;;
        --no-restart) NO_RESTART=true; shift ;;
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            EXPLICIT_ENV_FILE=true
            ENV_FILE="$2"
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        *) log_error "Unknown option: $1"; usage; exit 2 ;;
    esac
done

if [ "$EXPLICIT_SERVICE" != true ]; then
    UPDATE_GATEWAY=true
    UPDATE_KNOWLEDGE=true
fi

if [ "$EXPLICIT_ENV_FILE" = true ]; then
    require_env_file
fi

load_env

log_step "Pre-flight checks"
require_docker
require_env_file
assert_compose_owner

# Resolve and verify both Rust artifacts before copying or restarting any service.
if [ "$UPDATE_GATEWAY" = true ] && [ "$NO_RESTART" != true ]; then
    desired_runtime_image="${AI_PLATFORM_AGENT_RUNTIME_IMAGE:-$(agent_runtime_image_tag)}"
    desired_worker_image="${AGENT_CAPABILITY_WORKER_IMAGE:-$(agent_capability_worker_image_tag)}"
    assert_runtime_release_unit_locked "$desired_runtime_image" "$desired_worker_image"
    export AI_PLATFORM_AGENT_RUNTIME_IMAGE="$desired_runtime_image"
    export AGENT_CAPABILITY_WORKER_IMAGE="$desired_worker_image"
fi
COMPOSE_CMD=$(get_compose_cmd)
cd "$PROJECT_ROOT"

container_must_exist() {
    local container="$1"
    if ! docker inspect "$container" >/dev/null 2>&1; then
        log_error "Container is missing: $container. Create the local deployment first."
        exit 1
    fi
}

container_is_running() {
    [ "$(docker inspect -f '{{.State.Running}}' "$1" 2>/dev/null || true)" = "true" ]
}

site_packages() {
    local container="$1"
    local task_image task_site
    local probe='import site,sys; print(next((p for p in site.getsitepackages()+sys.path if p.endswith("site-packages")), ""))'
    container_must_exist "$container"
    if container_is_running "$container"; then
        task_site="$(docker exec "$container" python -c "$probe")"
    else
        # Use only the immutable image; never start the old app or forward its env.
        task_image="$(docker inspect -f '{{.Image}}' "$container")"
        task_site="$(docker run --rm --network none --entrypoint python "$task_image" -c "$probe")"
    fi
    task_site="${task_site//$'\r'/}"
    if [[ "$task_site" != /*/site-packages || "$task_site" == *$'\n'* ]]; then
        log_error "Cannot locate Python site-packages for $container"
        return 1
    fi
    printf '%s\n' "$task_site"
}

copy_dir() {
    local source="$1"
    local container="$2"
    local destination="$3"
    local owner="${4:-}"

    if [ ! -d "$source" ]; then
        log_error "Source directory not found: $source"
        exit 1
    fi

    container_must_exist "$container"
    if container_is_running "$container"; then
        docker exec -u root "$container" mkdir -p "$destination"
    fi
    # Existing /app and site-packages parents allow cp to create the leaf even
    # while the application is stopped for a schema transition.
    docker cp "$source/." "$container:$destination"
    if [ -n "$owner" ] && container_is_running "$container"; then
        docker exec -u root "$container" chown -R "$owner" "$destination" >/dev/null 2>&1 || true
    fi
    log_success "Copied $source -> $container:$destination"
}

copy_file() {
    local source="$1"
    local container="$2"
    local destination="$3"
    local owner="${4:-}"

    if [ ! -f "$source" ]; then
        log_error "Source file not found: $source"
        exit 1
    fi

    container_must_exist "$container"
    if container_is_running "$container"; then
        docker exec -u root "$container" mkdir -p "$(dirname "$destination")"
    fi
    docker cp "$source" "$container:$destination"
    if [ -n "$owner" ] && container_is_running "$container"; then
        docker exec -u root "$container" chown "$owner" "$destination" >/dev/null 2>&1 || true
    fi
    log_success "Copied $source -> $container:$destination"
}

warn_dependency_changes() {
    if ! command -v git >/dev/null 2>&1 || ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        return 0
    fi

    local changed
    changed="$(git diff --name-only -- \
        pyproject.toml uv.lock Dockerfile docker-compose.yml docker-compose.dev.yml \
        apps/knowledge-service/pyproject.toml apps/knowledge-service/Dockerfile \
        web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml web/Dockerfile 2>/dev/null || true)"
    if [ -n "$changed" ]; then
        log_warn "Dependency/Docker/build files changed; hot-update will not install dependencies or rebuild images."
        printf '%s\n' "$changed" | sed 's/^/  - /'
    fi
}

restart_services=()
UPDATE_AGENT_RUNTIME=false

log_step "Copying source into existing containers"
warn_dependency_changes

if [ "$UPDATE_GATEWAY" = true ]; then
    gateway="$(gateway_container)"
    gateway_site="$(site_packages "$gateway")"
    copy_dir "src" "$gateway" "/app/src" "appuser:appuser"
    copy_file \
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json" \
        "$gateway" "/app/src/core/data/platform_catalog_v1.json" "appuser:appuser"
    copy_dir "config" "$gateway" "/app/config" "appuser:appuser"
    copy_dir "database" "$gateway" "/app/database" "appuser:appuser"
    copy_dir "packages/ai-gateway-core/src/ai_gateway_core" "$gateway" "$gateway_site/ai_gateway_core"
    copy_dir "packages/ai-gateway-contracts/src/ai_gateway_contracts" "$gateway" "$gateway_site/ai_gateway_contracts"
    restart_services+=("gateway")
    UPDATE_AGENT_RUNTIME=true
fi

if [ "$UPDATE_KNOWLEDGE" = true ]; then
    knowledge="$(knowledge_container)"
    knowledge_worker="$(knowledge_worker_container)"
    knowledge_site="$(site_packages "$knowledge")"
    knowledge_worker_site="$(site_packages "$knowledge_worker")"
    copy_dir "apps/knowledge-service/src/knowledge_service" "$knowledge" "$knowledge_site/knowledge_service"
    copy_dir "packages/ai-gateway-core/src/ai_gateway_core" "$knowledge" "$knowledge_site/ai_gateway_core"
    copy_dir "packages/ai-gateway-contracts/src/ai_gateway_contracts" "$knowledge" "$knowledge_site/ai_gateway_contracts"
    copy_dir "apps/knowledge-service/src/knowledge_service" "$knowledge_worker" "$knowledge_worker_site/knowledge_service"
    copy_dir "packages/ai-gateway-core/src/ai_gateway_core" "$knowledge_worker" "$knowledge_worker_site/ai_gateway_core"
    copy_dir "packages/ai-gateway-contracts/src/ai_gateway_contracts" "$knowledge_worker" "$knowledge_worker_site/ai_gateway_contracts"
    restart_services+=("knowledge-service" "knowledge-worker")
fi

if [ "$UPDATE_FRONTEND" = true ]; then
    frontend="$(frontend_container)"
    container_must_exist "$frontend"
    log_step "Building frontend assets locally"
    pnpm -C web build
    copy_dir "web/dist" "$frontend" "/usr/share/nginx/html"
    frontend_entrypoint_stage="$(mktemp -d /tmp/ai-platform-hot-update-entrypoint.XXXXXX)"
    trap 'rm -rf -- "$frontend_entrypoint_stage"' EXIT
    cp -R web/docker-entrypoint.d/. "$frontend_entrypoint_stage/"
    chmod +x "$frontend_entrypoint_stage/40-runtime-config.sh"
    copy_dir "$frontend_entrypoint_stage" "$frontend" "/docker-entrypoint.d"
    # Restart runs the new entrypoint once with the container's configured env.
    restart_services+=("frontend")
fi

if [ "$NO_RESTART" = true ]; then
    log_warn "Skipped service restart because --no-restart was set."
else
    if [ "${#restart_services[@]}" -gt 0 ]; then
        log_step "Restarting updated services"
        # shellcheck disable=SC2086
        $COMPOSE_CMD --env-file "$ENV_FILE" restart "${restart_services[@]}"
    fi
    if [ "$UPDATE_AGENT_RUNTIME" = true ]; then
        current_runtime_image="$(docker inspect -f '{{.Image}}' "$(agent_runtime_container)" 2>/dev/null || true)"
        current_worker_image="$(docker inspect -f '{{.Image}}' "$(agent_capability_worker_container)" 2>/dev/null || true)"
        desired_runtime_digest="$(docker image inspect "$desired_runtime_image" --format '{{.Id}}')"
        desired_worker_digest="$(docker image inspect "$desired_worker_image" --format '{{.Id}}')"
        if [ "$current_runtime_image" != "$desired_runtime_digest" ] || [ "$current_worker_image" != "$desired_worker_digest" ]; then
            log_step "Recreating the locked Runtime and capability worker release unit"
            # shellcheck disable=SC2086
            $COMPOSE_CMD --env-file "$ENV_FILE" up -d --no-deps --force-recreate agent-runtime agent-capability-worker
        else
            # shellcheck disable=SC2086
            $COMPOSE_CMD --env-file "$ENV_FILE" restart agent-runtime agent-capability-worker
        fi
    fi
fi

if [ "$NO_RESTART" = true ]; then
    log_success "Source files copied. Services remain in their existing state."
    exit 0
fi

log_step "Runtime health checks"
if [ "$UPDATE_KNOWLEDGE" = true ]; then
    wait_for_healthy "Knowledge service" "check_knowledge_health" 60 || log_warn "Knowledge service may still be starting"
    wait_for_healthy "Knowledge worker" "check_knowledge_worker_health" 60 || log_warn "Knowledge worker may still be starting"
fi
if [ "$UPDATE_GATEWAY" = true ]; then
    wait_for_healthy "Gateway" "check_gateway_health" 60 || log_warn "Gateway may still be starting"
fi
if [ "$UPDATE_AGENT_RUNTIME" = true ]; then
    wait_for_healthy "Capability worker" "check_agent_capability_worker_health" 60 || log_warn "Capability worker may still be starting"
    wait_for_healthy "Agent Runtime" "check_agent_runtime_health" 60 || log_warn "Agent Runtime may still be starting"
fi
if [ "$UPDATE_FRONTEND" = true ]; then
    wait_for_healthy "Frontend" "check_frontend_health" 30 || log_warn "Frontend may still be starting"
fi

log_success "Hot update complete. No pip install or image rebuild was run."
