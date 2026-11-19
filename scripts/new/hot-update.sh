#!/bin/bash
# Native source updates compile images, then replace only the selected service containers.
set -euo pipefail
source "$(dirname "$0")/common.sh"
gateway=false; knowledge=false; frontend=false; no_restart=false; explicit=false
usage() { echo 'Usage: hot-update.sh [--gateway|--knowledge|--frontend|--all] [--no-restart] [--env FILE]'; }
while [[ $# -gt 0 ]]; do
    case "$1" in
        --gateway) gateway=true; explicit=true; shift;;
        --knowledge) knowledge=true; explicit=true; shift;;
        --frontend) frontend=true; explicit=true; shift;;
        --all) gateway=true; knowledge=true; frontend=true; explicit=true; shift;;
        --no-restart) no_restart=true; shift;;
        --env) [[ -n "${2:-}" && ! "${2:-}" =~ ^-- ]] || { usage; exit 2; }; ENV_FILE="$2"; shift 2;;
        -h|--help) usage; exit 0;;
        *) usage; exit 2;;
    esac
done
if [[ "$explicit" == false ]]; then gateway=true; knowledge=true; fi
load_env; require_env_file; require_docker; assert_compose_owner "$PROJECT_ROOT"
cd "$PROJECT_ROOT"
COMPOSE_CMD=$(get_compose_cmd)
services=()
[[ "$gateway" == false ]] || services+=(gateway)
if [[ "$knowledge" == true ]]; then
    services+=(knowledge-service)
    if topology_service_present knowledge-worker; then services+=(knowledge-worker); fi
fi
[[ "$frontend" == false ]] || services+=(frontend)
if [[ "$gateway" == true && "$no_restart" == false ]]; then
    export AI_PLATFORM_AGENT_RUNTIME_IMAGE="${AI_PLATFORM_AGENT_RUNTIME_IMAGE:-$(agent_runtime_image_tag)}"
    export AGENT_CAPABILITY_WORKER_IMAGE="${AGENT_CAPABILITY_WORKER_IMAGE:-$(agent_capability_worker_image_tag)}"
    assert_runtime_release_unit_locked "$AI_PLATFORM_AGENT_RUNTIME_IMAGE" "$AGENT_CAPABILITY_WORKER_IMAGE"
fi
# shellcheck disable=SC2086
COMPOSE_PARALLEL_LIMIT=1 $COMPOSE_CMD --env-file "$ENV_FILE" -f docker-compose.yml -f deploy/compose/docker-compose.build.yml build "${services[@]}"
if [[ "$no_restart" == true ]]; then
    log_success 'Native images built; running services were not replaced'
else
    replace_services=("${services[@]}")
    if [[ "$gateway" == true ]]; then
        replace_services+=(agent-runtime agent-capability-worker)
    fi
    # shellcheck disable=SC2086
    COMPOSE_PARALLEL_LIMIT=1 $COMPOSE_CMD --env-file "$ENV_FILE" -f docker-compose.yml -f deploy/compose/docker-compose.build.yml up -d --no-deps --force-recreate "${replace_services[@]}"
    if [[ "$gateway" == true ]]; then
        wait_for_healthy "Capability worker" check_agent_capability_worker_health 60
        wait_for_healthy "Agent Runtime" check_agent_runtime_health 60
        runtime_digest="$(platform-tools json-get "$PROJECT_ROOT/deploy/agent-runtime-source/lock.json" oci.artifacts.agent_runtime.image_digest)"
        worker_digest="$(platform-tools json-get "$PROJECT_ROOT/deploy/agent-runtime-source/lock.json" oci.artifacts.capability_worker.image_digest)"
        [[ "$(docker inspect "$(agent_runtime_container)" --format '{{.Image}}')" == "$runtime_digest" ]] || { log_error 'Running Runtime image differs from locked release'; exit 1; }
        worker_containers="$(compose_running_service_containers agent-capability-worker)"
        [[ -n "$worker_containers" ]] || { log_error 'No capability worker container is running'; exit 1; }
        while IFS= read -r container; do
            [[ "$(docker inspect "$container" --format '{{.Image}}')" == "$worker_digest" ]] || { log_error 'Running capability worker image differs from locked release'; exit 1; }
        done <<< "$worker_containers"
        assert_runtime_release_unit_locked "$AI_PLATFORM_AGENT_RUNTIME_IMAGE" "$AGENT_CAPABILITY_WORKER_IMAGE"
    fi
    if [[ "$knowledge" == true ]]; then
        wait_for_healthy "Knowledge service" check_knowledge_health 60
        if topology_service_present knowledge-worker; then
            wait_for_healthy "Knowledge worker" check_knowledge_worker_health 60
        fi
    fi
    if [[ "$gateway" == true ]]; then wait_for_healthy "Gateway" check_gateway_health 60; fi
    if [[ "$frontend" == true ]]; then wait_for_healthy "Frontend" check_frontend_health 30; fi
    log_success 'Selected native service containers updated'
fi
