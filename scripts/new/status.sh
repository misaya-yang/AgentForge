#!/bin/bash
# =============================================================================
# AI Gateway - Service Status & Health Check
# =============================================================================
# Usage:  make status
#   or:   ./scripts/new/status.sh
# =============================================================================

source "$(dirname "$0")/common.sh"
load_env

COMPOSE_CMD=$(get_compose_cmd)
TOPOLOGY_MODE="${AI_PLATFORM_TOPOLOGY_MODE:-full}"
cd "$PROJECT_ROOT"
if ! python3 "$PROJECT_ROOT/scripts/deploy/topology_modes.py" --mode "$TOPOLOGY_MODE" >/dev/null; then
    log_error "Invalid AI_PLATFORM_TOPOLOGY_MODE=$TOPOLOGY_MODE"
    exit 2
fi

echo ""
echo "=== Docker Compose Services ==="
# shellcheck disable=SC2086
if ! $COMPOSE_CMD -f docker-compose.yml -f "docker-compose.${TOPOLOGY_MODE}.yml" \
    --env-file "$ENV_FILE" ps 2>/dev/null; then
    echo "  (compose metadata unavailable; falling back to known container names)"
    docker ps --filter "name=ai-gateway-" --format "  {{.Names}}\t{{.Status}}" 2>/dev/null || true
fi

echo ""
echo "=== Health Checks ==="

STATUS_FAILURES=0

container_is_healthy() {
    local container="$1"
    local status
    status="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$container" 2>/dev/null || true)"
    [ "$status" = "healthy" ] || [ "$status" = "running" ]
}

check_and_report() {
    local name="$1"
    local check_fn="$2"
    printf "  %-18s " "$name:"
    if $check_fn 2>/dev/null; then
        echo -e "${GREEN}Healthy${NC}"
    else
        echo -e "${RED}Not available${NC}"
        STATUS_FAILURES=$((STATUS_FAILURES + 1))
    fi
}

check_postgres_status() {
    check_postgres_health || container_is_healthy "$(pg_container)"
}

check_redis_status() {
    check_redis_health || container_is_healthy "$(redis_container)"
}

check_and_report "PostgreSQL" check_postgres_status
check_and_report "Redis" check_redis_status
check_and_report "Qdrant" check_qdrant_health
check_and_report "Knowledge" check_knowledge_health
if topology_service_present knowledge-worker; then
    check_and_report "Knowledge worker" check_knowledge_worker_health
else
    printf "  %-18s %s\n" "Knowledge worker:" "Integrated (compact mode)"
fi
check_and_report "Gateway" check_gateway_health
check_and_report "Gateway metrics" check_gateway_metrics
check_and_report "Frontend" check_frontend_health
check_and_report "Agent Runtime" check_agent_runtime_health
check_and_report "Capability worker" check_agent_capability_worker_health
check_and_report "Topology replicas" check_topology_cardinality

echo ""

if [ "$STATUS_FAILURES" -gt 0 ]; then
    log_error "$STATUS_FAILURES health check(s) failed."
    exit 1
fi
