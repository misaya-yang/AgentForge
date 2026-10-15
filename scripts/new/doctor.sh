#!/bin/bash
# =============================================================================
# AI Gateway - Environment Doctor
# =============================================================================
# Read-only preflight. Run this before `make quickstart` on a new machine, and
# whenever the stack behaves unexpectedly. It never starts, stops, builds, or
# mutates anything.
#
# Usage:  make doctor [ENV_FILE=/path/to/.env]
#   or:   ./scripts/new/doctor.sh [--env FILE]
#
# Exit codes: 0 = ready (warnings allowed), 1 = blocking problem found.
# =============================================================================

source "$(dirname "$0")/common.sh"

while [[ $# -gt 0 ]]; do
    case $1 in
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            ENV_FILE="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [--env FILE]"
            exit 0 ;;
        *) log_error "Unknown option: $1"; exit 1 ;;
    esac
done

PROBLEMS=0
ADVISORIES=0

ok()      { log_success "$1"; }
problem() { log_error "$1"; PROBLEMS=$((PROBLEMS + 1)); }
advise()  { log_warn "$1"; ADVISORIES=$((ADVISORIES + 1)); }

# -- 1. Required tooling ------------------------------------------------------
log_step "Required tooling"

for tool in docker make openssl curl; do
    if command -v "$tool" >/dev/null 2>&1; then
        ok "$tool found"
    else
        problem "$tool is not installed"
    fi
done

if docker compose version >/dev/null 2>&1; then
    ok "docker compose v2 available"
elif command -v docker-compose >/dev/null 2>&1; then
    advise "only legacy docker-compose found; Compose v2 is expected"
else
    problem "Docker Compose is not available"
fi

if docker info >/dev/null 2>&1; then
    ok "Docker daemon is running"
    DOCKER_UP=true
else
    problem "Docker daemon is not running"
    DOCKER_UP=false
fi

# -- 2. Optional developer tooling -------------------------------------------
log_step "Developer tooling (needed only to build or test from source)"

for tool in git uv node pnpm; do
    if command -v "$tool" >/dev/null 2>&1; then
        ok "$tool found"
    else
        advise "$tool not found (only required for source builds and tests)"
    fi
done

# -- 3. Host capacity ---------------------------------------------------------
log_step "Host capacity"

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64|arm64|aarch64) ok "architecture $ARCH is supported by the published images" ;;
    *) advise "architecture $ARCH has no published image; a source build is required" ;;
esac

if [ "$DOCKER_UP" = true ]; then
    MEM_BYTES="$(docker info --format '{{.MemTotal}}' 2>/dev/null || echo 0)"
    if [[ "$MEM_BYTES" =~ ^[0-9]+$ ]] && [ "$MEM_BYTES" -gt 0 ]; then
        MEM_GIB=$((MEM_BYTES / 1073741824))
        if [ "$MEM_GIB" -ge 4 ]; then
            ok "Docker has ${MEM_GIB} GiB of memory available"
        else
            advise "Docker has ${MEM_GIB} GiB of memory; the full stack expects about 4 GiB. Keep COMPOSE_PARALLEL_LIMIT=1 and start services serially."
        fi
    else
        advise "could not read Docker memory limit"
    fi
fi

DISK_FREE_KB="$(df -Pk "$PROJECT_ROOT" 2>/dev/null | awk 'NR==2 {print $4}')"
if [[ "$DISK_FREE_KB" =~ ^[0-9]+$ ]]; then
    DISK_FREE_GIB=$((DISK_FREE_KB / 1048576))
    if [ "$DISK_FREE_GIB" -ge 20 ]; then
        ok "${DISK_FREE_GIB} GiB free on the repository volume"
    else
        advise "${DISK_FREE_GIB} GiB free on the repository volume; images and volumes need roughly 20 GiB"
    fi
fi

# -- 4. Compose ownership -----------------------------------------------------
# Containers are shared by name across checkouts. Acting on another checkout's
# stack is the most damaging mistake possible here, so it blocks.
log_step "Docker Compose ownership"

if [ "$DOCKER_UP" = true ]; then
    OWNERSHIP_CONFLICT=false
    STACK_RUNNING=false
    for container in \
        "$(pg_container)" "$(redis_container)" "$(qdrant_container)" \
        "$(gateway_container)" "$(frontend_container)" \
        "$(knowledge_container)" "$(agent_runtime_container)"; do
        docker inspect "$container" >/dev/null 2>&1 || continue
        STACK_RUNNING=true
        owner="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project.working_dir" }}' "$container" 2>/dev/null || true)"
        project="$(docker inspect -f '{{ index .Config.Labels "com.docker.compose.project" }}' "$container" 2>/dev/null || true)"
        if [ -z "$owner" ]; then
            # `make dev-setup` creates pg/redis/qdrant with plain `docker run`, so they
            # legitimately carry no Compose labels. Surface it, but do not block: the
            # real guard before any mutation is assert_compose_owner() in common.sh.
            advise "container '$container' has no Compose label — expected after 'make dev-setup'; confirm it is yours before deploying"
        elif [ "$owner" != "$PROJECT_ROOT" ]; then
            problem "container '$container' belongs to another checkout: $owner"
            OWNERSHIP_CONFLICT=true
        elif [ "$project" != "ai-gateway" ]; then
            problem "container '$container' belongs to another Compose project: ${project:-unlabeled}"
            OWNERSHIP_CONFLICT=true
        fi
    done

    for legacy in ai-gateway-knowledge islamic-content-service; do
        if docker inspect "$legacy" >/dev/null 2>&1; then
            advise "legacy container '$legacy' exists; it usually belongs to a different checkout"
        fi
    done

    if [ "$OWNERSHIP_CONFLICT" = true ]; then
        log_error "Stop or remove the other checkout's compose project before deploying from $PROJECT_ROOT."
    elif [ "$STACK_RUNNING" = true ]; then
        ok "no wrong-checkout ai-gateway containers found"
    else
        ok "no ai-gateway containers exist yet"
    fi
else
    advise "skipped ownership check because Docker is not running"
    STACK_RUNNING=false
fi

# -- 5. Environment file ------------------------------------------------------
log_step "Environment file"

ENV_PATH="$(env_file_path)"
if [ -f "$ENV_PATH" ]; then
    ok "env file present: $ENV_PATH"
    MODE="$(stat -f '%OLp' "$ENV_PATH" 2>/dev/null || stat -c '%a' "$ENV_PATH" 2>/dev/null || echo '')"
    if [ -n "$MODE" ] && [ "$MODE" != "600" ]; then
        advise "env file mode is $MODE; 600 is expected for a file holding secrets"
    fi
    load_env
    # Report the offending keys, never their values.
    PLACEHOLDER_KEYS="$(grep -oE '^[A-Za-z_][A-Za-z0-9_]*=[^=]*change_me' "$ENV_PATH" 2>/dev/null | cut -d= -f1 | sort -u | tr '\n' ' ' || true)"
    if [ -n "$PLACEHOLDER_KEYS" ]; then
        problem "env keys still hold change_me placeholders: ${PLACEHOLDER_KEYS%% }"
    fi
    # Presence only. Values are never read into output.
    if [ -n "${DASHSCOPE_CHAT_API_KEY:-${DASHSCOPE_API_KEY:-}}" ]; then
        ok "a DashScope/Qwen chat key is configured"
    else
        advise "no DASHSCOPE_API_KEY set; the stack still starts and a provider can be added in the web console (MODEL_SETUP_MODE=ui)"
    fi
else
    advise "no env file at $ENV_PATH; 'make quickstart' will generate one with fresh local secrets"
fi

if [ -n "${OPENAI_API_KEY:-}" ]; then
    advise "OPENAI_API_KEY is set in your shell and overrides .env during Compose interpolation. Start with 'env -u OPENAI_API_KEY make quickstart' if it is stale."
fi

# -- 6. Host ports ------------------------------------------------------------
log_step "Host ports"

port_in_use() {
    local port="$1"
    if command -v lsof >/dev/null 2>&1; then
        lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
    elif command -v nc >/dev/null 2>&1; then
        nc -z 127.0.0.1 "$port" >/dev/null 2>&1
    else
        return 1
    fi
}

check_port() {
    local port="$1" label="$2"
    if port_in_use "$port"; then
        if [ "$STACK_RUNNING" = true ]; then
            log_info "port $port ($label) is in use — this project's stack is already running"
        else
            advise "port $port ($label) is already in use; change it in .env or free the port"
        fi
    else
        ok "port $port ($label) is free"
    fi
}

check_port "${GATEWAY_PORT:-8080}" "gateway"
check_port "${FRONTEND_PORT:-8081}" "frontend"
check_port "${KNOWLEDGE_SERVICE_PORT:-8092}" "knowledge service"
check_port "${POSTGRES_PORT:-5432}" "postgres"
check_port "${REDIS_PORT:-6379}" "redis"
check_port "${QDRANT_HTTP_PORT:-6333}" "qdrant http"

# -- Summary ------------------------------------------------------------------
echo ""
if [ "$PROBLEMS" -gt 0 ]; then
    log_error "$PROBLEMS blocking problem(s), $ADVISORIES advisory item(s)."
    echo "Fix the blocking items above, then re-run: make doctor"
    exit 1
fi

log_success "No blocking problems. $ADVISORIES advisory item(s)."
if [ -f "$ENV_PATH" ]; then
    echo "Next: make validate-config  ->  make quickstart  ->  make status"
else
    echo "Next: make quickstart  (generates .env, pulls images, starts, migrates, validates)"
fi
