#!/bin/bash
# =============================================================================
# AI Gateway - Environment and Runtime Validator
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

ENV_FILE="${ENV_FILE:-${PROJECT_ROOT}/.env}"
RUNTIME_CHECK=false
CONFIG_CHECK=true
INFRA_ONLY=false
EXAMPLE_CHECK=false
ERRORS=0
WARNINGS=0

usage() {
    cat <<'USAGE'
Usage: scripts/new/validate-env.sh [--env FILE] [--config-only] [--runtime] [--infra-only] [--example]

Checks the local .env file without printing secret values.

Modes:
  --config-only   Validate required variables and value shape only (default)
  --runtime       Validate config, then verify Docker runtime dependencies
  --infra-only    Validate only PostgreSQL, Redis, and Qdrant requirements
  --example       Validate a committed example env file for public shape and compose rendering
  --env FILE      Use a specific env file instead of .env
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            ENV_FILE="$2"
            shift 2
            ;;
        --config-only)
            CONFIG_CHECK=true
            RUNTIME_CHECK=false
            shift
            ;;
        --runtime)
            CONFIG_CHECK=true
            RUNTIME_CHECK=true
            shift
            ;;
        --infra-only)
            INFRA_ONLY=true
            shift
            ;;
        --example)
            EXAMPLE_CHECK=true
            CONFIG_CHECK=true
            RUNTIME_CHECK=false
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown argument: $1"
            usage
            exit 2
            ;;
    esac
done

if [ "$EXAMPLE_CHECK" = true ]; then
    RUNTIME_CHECK=false
    INFRA_ONLY=false
fi

fail() {
    log_error "$1"
    ERRORS=$((ERRORS + 1))
}

warn() {
    log_warn "$1"
    WARNINGS=$((WARNINGS + 1))
}

strip_optional_quotes() {
    local value="$1"
    value="${value%$'\r'}"
    if [[ "$value" =~ ^\".*\"$ ]] || [[ "$value" =~ ^\'.*\'$ ]]; then
        value="${value:1:${#value}-2}"
    fi
    printf '%s' "$value"
}

load_env_file() {
    if [ ! -r "$ENV_FILE" ]; then
        fail "Missing env file: $ENV_FILE"
        return
    fi

    while IFS= read -r line || [ -n "$line" ]; do
        [[ "$line" =~ ^[[:space:]]*$ ]] && continue
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ "$line" != *"="* ]] && continue

        local key="${line%%=*}"
        local value="${line#*=}"
        key=$(echo "$key" | xargs)

        if [[ "$key" =~ ^[a-zA-Z_][a-zA-Z0-9_]*$ ]]; then
            value="$(strip_optional_quotes "$value")"
            export "$key=$value"
        fi
    done < "$ENV_FILE"
}

env_value() {
    local key="$1"
    printf '%s' "${!key:-}"
}

env_file_has_key() {
    local key="$1"
    grep -Eq "^[[:space:]]*${key}[[:space:]]*=" "$ENV_FILE"
}

require_example_key() {
    local key="$1"
    if ! env_file_has_key "$key"; then
        fail "$key must be declared in the example env file."
    fi
}

require_example_value() {
    local key="$1"
    require_example_key "$key"
    if [ -z "$(env_value "$key")" ]; then
        fail "$key must have an example value."
    fi
}

is_placeholder() {
    local raw="$1"
    local value
    value="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]')"

    case "$value" in
        ""|change_me*|changeme*|change-me*|replace_me*|replace-me*|your_*|your-*|*_here|example|example_*|demo|test|password|secret|123456|111111)
            return 0
            ;;
    esac

    return 1
}

require_secret() {
    local key="$1"
    local min_length="$2"
    local value
    value="$(env_value "$key")"

    if is_placeholder "$value"; then
        fail "$key must be set to a non-placeholder secret."
        return
    fi

    if [ "${#value}" -lt "$min_length" ]; then
        fail "$key must be at least ${min_length} characters."
    fi
}

require_secret_or_local_default() {
    local key="$1"
    local min_length="$2"
    local local_default="$3"
    local value
    value="$(env_value "$key")"

    if [ "$value" = "$local_default" ]; then
        warn "$key uses the documented local bootstrap default; rotate it before shared or non-local deployments."
        return
    fi

    require_secret "$key" "$min_length"
}

require_non_empty() {
    local key="$1"
    local value
    value="$(env_value "$key")"

    if is_placeholder "$value"; then
        fail "$key must be set."
    fi
}

require_versioned_image() {
    local key="$1"
    local default_value="${2:-}"
    local value
    value="$(env_value "$key")"
    value="${value:-$default_value}"

    if is_placeholder "$value"; then
        fail "$key must be set to a published image reference."
        return
    fi

    if [[ "$value" =~ @sha256:[0-9a-fA-F]{64}$ ]]; then
        return
    fi

    if [[ "$value" =~ :v?[0-9]+\.[0-9]+\.[0-9]+([._-][A-Za-z0-9][A-Za-z0-9._-]*)?$ ]]; then
        return
    fi

    if [[ "$value" =~ ^ai-gateway-agent-(runtime|capability-worker):local-[0-9a-f]{12}-[0-9a-f]{12}$ ]]; then
        return
    fi

    fail "$key must use a full semantic release tag (for example 2.0.0) or sha256 digest."
}

require_positive_int() {
    local key="$1"
    local default_value="${2:-}"
    local value
    value="$(env_value "$key")"
    value="${value:-$default_value}"

    if ! [[ "$value" =~ ^[0-9]+$ ]] || [ "$value" -le 0 ]; then
        fail "$key must be a positive integer."
    fi
}

require_url() {
    local key="$1"
    local value
    value="$(env_value "$key")"
    [ -z "$value" ] && return

    if ! [[ "$value" =~ ^https?:// ]]; then
        fail "$key must start with http:// or https://."
    fi
}

is_non_local_auth_domain() {
    local auth_domain
    auth_domain="$(env_value AUTH_ALLOWED_EMAIL_DOMAIN)"
    ! is_placeholder "$auth_domain" && [ "$auth_domain" != "example.com" ]
}

normalize_domain() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

require_frontend_auth_domain_alignment() {
    local auth_domain frontend_domain
    auth_domain="$(env_value AUTH_ALLOWED_EMAIL_DOMAIN)"
    frontend_domain="$(env_value VITE_AUTH_EMAIL_DOMAIN)"

    [ -z "$frontend_domain" ] && return

    if is_non_local_auth_domain &&
        [ "$(normalize_domain "$frontend_domain")" != "$(normalize_domain "$auth_domain")" ]; then
        fail "VITE_AUTH_EMAIL_DOMAIN must match AUTH_ALLOWED_EMAIL_DOMAIN when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    fi
}

email_domain() {
    local value="$1"
    printf '%s' "${value##*@}" | tr '[:upper:]' '[:lower:]'
}

require_support_email_release_ready() {
    local support_email
    support_email="$(env_value VITE_SUPPORT_EMAIL)"

    [ -z "$support_email" ] && return

    if ! [[ "$support_email" =~ ^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$ ]]; then
        fail "VITE_SUPPORT_EMAIL must be a valid email address when set."
        return
    fi

    if is_non_local_auth_domain && [ "$(email_domain "$support_email")" = "example.com" ]; then
        fail "VITE_SUPPORT_EMAIL must not use example.com when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    fi
}

require_frontend_runtime_url() {
    local key="$1"
    local value
    value="$(env_value "$key")"
    [ -z "$value" ] && return

    if [[ "$value" =~ ^/[^/] ]]; then
        return
    fi

    if ! [[ "$value" =~ ^https?:// ]]; then
        fail "$key must be empty, a same-origin path, or an http(s) URL."
        return
    fi

    if is_non_local_auth_domain; then
        if [[ ! "$value" =~ ^https:// ]]; then
            fail "$key must use https:// or a same-origin path when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
        fi
        if [[ "$value" =~ ^https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])([:/]|$) ]]; then
            fail "$key must not use localhost or loopback when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
        fi
    fi
}

require_domain() {
    local key="$1"
    local value
    value="$(env_value "$key")"

    if is_placeholder "$value"; then
        fail "$key must be set."
        return
    fi

    if ! [[ "$value" =~ ^[A-Za-z0-9.-]+\.[A-Za-z]{2,}$ ]]; then
        fail "$key must be a valid domain name."
    fi
}

require_json_array_of_strings() {
    local key="$1"
    local value
    value="$(env_value "$key")"
    [ -z "$value" ] && return

    if ! [[ "$value" =~ ^\[[[:space:]]*\"[^\"]*\"([[:space:]]*,[[:space:]]*\"[^\"]*\")*[[:space:]]*\]$ ]]; then
        fail "$key must be a JSON array of strings."
    fi
}

require_cors_origins() {
    local key="$1"
    local value auth_domain
    value="$(env_value "$key")"

    if [ -z "$value" ]; then
        fail "$key must be set to a JSON array of explicit http(s) origins."
        return
    fi

    if [[ "$value" == *"*"* ]]; then
        fail "$key must not use wildcard origins."
        return
    fi

    if ! [[ "$value" =~ ^\[[[:space:]]*\"https?://[A-Za-z0-9.-]+(:[0-9]+)?\"([[:space:]]*,[[:space:]]*\"https?://[A-Za-z0-9.-]+(:[0-9]+)?\")*[[:space:]]*\]$ ]]; then
        fail "$key must be a JSON array of explicit http(s) origins."
        return
    fi

    if is_non_local_auth_domain; then
        if [[ "$value" == *"\"http://"* ]]; then
            fail "$key must use https origins when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
        fi
        if [[ "$value" == *"localhost"* ]] || [[ "$value" == *"127.0.0.1"* ]] || [[ "$value" == *"0.0.0.0"* ]]; then
            fail "$key must not include localhost origins when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
        fi
    fi
}

has_any_key() {
    local key
    for key in "$@"; do
        if ! is_placeholder "$(env_value "$key")"; then
            return 0
        fi
    done
    return 1
}

allows_ui_model_setup() {
    [ "${MODEL_SETUP_MODE:-ui}" = "ui" ]
}

validate_embedding_provider_key() {
    local provider="$1"

    case "$provider" in
        dashscope)
            if ! has_any_key KB_EMBEDDING_API_KEY DASHSCOPE_API_KEY; then
                if allows_ui_model_setup; then
                    log_warn "DashScope embeddings are not configured yet; the UI can still open for model setup."
                else
                    fail "DashScope embeddings require DASHSCOPE_API_KEY or a dedicated KB_EMBEDDING_API_KEY."
                fi
            fi
            ;;
        gemini)
            if ! has_any_key KB_EMBEDDING_API_KEY GEMINI_API_KEY GOOGLE_API_KEY; then
                if allows_ui_model_setup; then
                    log_warn "Gemini embeddings are not configured yet; the UI can still open for model setup."
                else
                    fail "Gemini embeddings require GEMINI_API_KEY, GOOGLE_API_KEY, or a dedicated KB_EMBEDDING_API_KEY."
                fi
            fi
            ;;
        siliconflow)
            if ! has_any_key KB_EMBEDDING_API_KEY SILICONFLOW_API_KEY; then
                if allows_ui_model_setup; then
                    log_warn "SiliconFlow embeddings are not configured yet; the UI can still open for model setup."
                else
                    fail "SiliconFlow embeddings require SILICONFLOW_API_KEY or a dedicated KB_EMBEDDING_API_KEY."
                fi
            fi
            ;;
    esac
}

validate_ports() {
    local keys=(
        POSTGRES_PORT
        REDIS_PORT
        QDRANT_HTTP_PORT
        QDRANT_GRPC_PORT
        GATEWAY_PORT
        FRONTEND_PORT
        KNOWLEDGE_SERVICE_PORT
    )
    local defaults=(
        5432
        6379
        6333
        6334
        8080
        8081
        8092
    )
    local seen=""
    local selected="${*:-}"
    local index=0
    local key value

    for key in "${keys[@]}"; do
        if [ -n "$selected" ] && [[ " $selected " != *" $key "* ]]; then
            index=$((index + 1))
            continue
        fi

        value="$(env_value "$key")"
        value="${value:-${defaults[$index]}}"
        index=$((index + 1))

        if ! [[ "$value" =~ ^[0-9]+$ ]] || [ "$value" -lt 1 ] || [ "$value" -gt 65535 ]; then
            fail "$key must be a TCP port between 1 and 65535."
            continue
        fi

        if [[ " $seen " == *" $value "* ]]; then
            fail "$key uses duplicate host port $value."
        fi
        seen="$seen $value"
    done
}

validate_compose_config() {
    local compose_cmd
    local no_interpolate=false
    local topology_mode="${AI_PLATFORM_TOPOLOGY_MODE:-full}"

    if [ "$ERRORS" -gt 0 ]; then
        log_warn "Skipping docker compose config validation until earlier configuration errors are fixed."
        return
    fi

    compose_cmd="$(get_compose_cmd)"
    if ! python3 "$PROJECT_ROOT/scripts/deploy/topology_modes.py" --mode "$topology_mode" >/dev/null; then
        fail "AI_PLATFORM_TOPOLOGY_MODE must resolve one of compact/full/scale."
        return
    fi

    if [ "${1:-}" = "--no-interpolate" ]; then
        no_interpolate=true
        shift
    fi

    if [ "$no_interpolate" = true ]; then
        if ! (cd "$PROJECT_ROOT" && $compose_cmd -f docker-compose.yml \
            -f "docker-compose.${topology_mode}.yml" --env-file "$ENV_FILE" \
            config --quiet --no-interpolate "$@"); then
            fail "docker compose config validation failed."
        fi
        return
    fi

    if ! (cd "$PROJECT_ROOT" && $compose_cmd -f docker-compose.yml \
        -f "docker-compose.${topology_mode}.yml" --env-file "$ENV_FILE" config --quiet "$@"); then
        fail "docker compose config validation failed."
    fi
}

configured_chat_providers() {
    local providers=""
    local google_backend="${GOOGLE_CHAT_BACKEND:-${GOOGLE_API_BACKEND:-ai_studio}}"
    google_backend="$(printf '%s' "$google_backend" | tr '[:upper:]' '[:lower:]')"

    if has_any_key OPENAI_API_KEY; then
        providers="${providers} openai"
    fi
    if has_any_key ANTHROPIC_API_KEY; then
        providers="${providers} anthropic"
    fi
    if has_any_key DEEPSEEK_API_KEY; then
        providers="${providers} deepseek"
    fi
    if has_any_key DASHSCOPE_CHAT_API_KEY DASHSCOPE_API_KEY; then
        providers="${providers} dashscope"
    fi

    # Mirror ai_gateway_core.config.resolve_google("chat") plus the
    # Gateway provider bootstrap:
    # - AI Studio backend uses GEMINI_API_KEY / GOOGLE_API_KEY for `google`.
    # - Vertex backend uses VERTEX_CHAT_API_KEY -> VERTEX_API_KEY -> studio key
    #   fallback, and Gateway configures both `google` with backend
    #   vertex and `google-vertex`.
    # - With AI Studio backend, VERTEX_API_KEY alone configures only
    #   `google-vertex`; VERTEX_CHAT_API_KEY alone is ignored by runtime.
    if [ "$google_backend" = "vertex" ]; then
        if has_any_key VERTEX_CHAT_API_KEY VERTEX_API_KEY GEMINI_API_KEY GOOGLE_API_KEY; then
            providers="${providers} google google-vertex"
        fi
    else
        if has_any_key GEMINI_API_KEY GOOGLE_API_KEY; then
            providers="${providers} google"
        fi
        if has_any_key VERTEX_API_KEY; then
            providers="${providers} google-vertex"
        fi
    fi

    printf '%s\n' "$providers" | xargs
}

provider_in_list() {
    local needle="$1"
    local haystack="$2"
    [[ " $haystack " == *" $needle "* ]]
}

query_enabled_model_providers() {
    local sql="
        SELECT provider_id || '|' || COUNT(*)
        FROM llm_models
        WHERE tenant_id = 'default' AND is_enabled = true
        GROUP BY provider_id
        ORDER BY provider_id;
    "

    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        docker exec -i "$(pg_container)" psql -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    elif command -v psql &>/dev/null; then
        PGPASSWORD="$(pg_password)" psql -h "$(pg_host)" -p "$(pg_port)" \
            -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    else
        return 1
    fi
}

query_database_credential_providers() {
    local sql="
        SELECT DISTINCT m.provider_id
        FROM llm_models AS m
        JOIN llm_providers AS p
          ON p.tenant_id = m.tenant_id
         AND p.provider_id = m.provider_id
        WHERE m.tenant_id = 'default'
          AND m.is_enabled = true
          AND p.is_enabled = true
          AND COALESCE(p.api_key_encrypted, '') <> ''
        ORDER BY m.provider_id;
    "

    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        docker exec -i "$(pg_container)" psql -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    elif command -v psql &>/dev/null; then
        PGPASSWORD="$(pg_password)" psql -h "$(pg_host)" -p "$(pg_port)" \
            -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    else
        return 1
    fi
}

query_enabled_default_qwen_model() {
    local sql="
        SELECT COUNT(*)
        FROM llm_models
        WHERE tenant_id = 'default'
          AND provider_id = 'dashscope'
          AND model_id = 'qwen3.7-plus'
          AND is_enabled = true;
    "

    if docker ps --format '{{.Names}}' | grep -q "^$(pg_container)$" 2>/dev/null; then
        docker exec -i "$(pg_container)" psql -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    elif command -v psql &>/dev/null; then
        PGPASSWORD="$(pg_password)" psql -h "$(pg_host)" -p "$(pg_port)" \
            -U "$(pg_user)" -d "$(pg_database)" -Atc "$sql" 2>/dev/null
    else
        return 1
    fi
}

validate_chat_model_alignment() {
    local configured database_configured providers_summary rows available_count enabled_summary provider count

    configured="$(configured_chat_providers)"
    database_configured="$(query_database_credential_providers)" || database_configured=""
    configured="$(printf '%s\n%s\n' "$configured" "$database_configured" | xargs)"
    rows="$(query_enabled_model_providers)" || {
        fail "Unable to query llm_models for chat model/provider alignment."
        return
    }

    if [ -z "$rows" ]; then
        fail "No enabled chat models found in llm_models for tenant default."
        return
    fi

    available_count=0
    enabled_summary=""
    while IFS='|' read -r provider count; do
        [ -z "$provider" ] && continue
        enabled_summary="${enabled_summary}${enabled_summary:+, }${provider}:${count}"
        if provider_in_list "$provider" "$configured"; then
            available_count=$((available_count + count))
        fi
    done <<< "$rows"

    providers_summary="${configured:-none}"
    if [ "$available_count" -le 0 ]; then
        if allows_ui_model_setup; then
            log_warn "Chat model provider is not configured yet. Open the frontend provider setup, then rerun make validate. Enabled model providers: ${enabled_summary}."
        else
            fail "No enabled chat models match configured providers. Configured providers: ${providers_summary}. Enabled model providers: ${enabled_summary}."
        fi
        return
    fi

    log_success "Chat model/provider alignment is valid (${available_count} available model(s))."
}

validate_config() {
    log_step "Validating configuration"
    load_env_file

    if [ "$ERRORS" -gt 0 ]; then
        return
    fi

    require_secret POSTGRES_PASSWORD 8
    require_secret REDIS_PASSWORD 8
    require_positive_int POSTGRES_PORT 5432
    require_positive_int REDIS_PORT 6379
    require_positive_int QDRANT_HTTP_PORT 6333
    require_positive_int QDRANT_GRPC_PORT 6334

    if [ "$INFRA_ONLY" = true ]; then
        validate_ports POSTGRES_PORT REDIS_PORT QDRANT_HTTP_PORT QDRANT_GRPC_PORT
        validate_compose_config --no-interpolate postgres redis qdrant
        if [ "$ERRORS" -eq 0 ]; then
            log_success "Infrastructure configuration validation passed"
        fi
        return
    fi

    require_secret JWT_SECRET 32
    require_secret GATEWAY_ENCRYPTION_KEY 32
    require_secret AI_PLATFORM_INTERNAL_TOKEN 32
    require_secret AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET 32
    require_secret AI_PLATFORM_CAPABILITY_PROOF_SECRET 32
    require_secret AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN 32
    require_secret_or_local_default DEFAULT_USER_PASSWORD 12 "ChangeMe-Admin-2026!"
    require_domain AUTH_ALLOWED_EMAIL_DOMAIN
    require_frontend_auth_domain_alignment
    require_support_email_release_ready

    require_positive_int GATEWAY_PORT 8080
    require_positive_int FRONTEND_PORT 8081
    require_positive_int KNOWLEDGE_SERVICE_PORT 8092
    require_positive_int RATE_LIMIT_IP_LIMIT 500
    require_positive_int RATE_LIMIT_ANONYMOUS_LIMIT 200
    require_positive_int RATE_LIMIT_NORMAL_LIMIT 300
    require_positive_int RATE_LIMIT_PREMIUM_LIMIT 1000
    require_positive_int RATE_LIMIT_ENTERPRISE_LIMIT 5000
    require_positive_int RATE_LIMIT_ADMIN_LIMIT 10000
    require_positive_int RATE_LIMIT_ASSISTANT_CHAT_LIMIT 240

    validate_ports

    local embedding_provider="${KB_EMBEDDING_PROVIDER:-dashscope}"
    case "${MODEL_SETUP_MODE:-ui}" in
        ui|environment)
            ;;
        *)
            fail "MODEL_SETUP_MODE must be one of: ui, environment."
            ;;
    esac
    # Optional format check: DEFAULT_MODEL is never required, but when set it
    # must be non-empty (the server default model for omitted model_id).
    if env_file_has_key DEFAULT_MODEL && [ -z "$(env_value DEFAULT_MODEL)" ]; then
        fail "DEFAULT_MODEL must be non-empty when set."
    fi
    case "$embedding_provider" in
        gemini|dashscope|siliconflow)
            ;;
        *)
            fail "KB_EMBEDDING_PROVIDER must be one of: gemini, dashscope, siliconflow."
            ;;
    esac

    validate_embedding_provider_key "$embedding_provider"
    require_positive_int KB_EMBEDDING_DIMENSION 1024

    require_versioned_image GATEWAY_IMAGE "ghcr.io/misaya-yang/ai-gateway:2.0.0"
    require_versioned_image FRONTEND_IMAGE "ghcr.io/misaya-yang/ai-gateway-web:2.0.0"
    require_versioned_image KNOWLEDGE_IMAGE "ghcr.io/misaya-yang/ai-gateway-knowledge-service:2.0.0"
    require_versioned_image MIGRATE_IMAGE "ghcr.io/misaya-yang/ai-gateway-migrate:2.0.0"

    if [ -z "$(configured_chat_providers)" ]; then
        if allows_ui_model_setup; then
            log_warn "No environment chat provider is configured; starting in UI model-setup mode."
        else
            fail "Set at least one supported chat provider key before starting in environment setup mode."
        fi
    fi

    require_url KB_SERVICE_URL
    require_frontend_runtime_url VITE_API_URL
    require_frontend_runtime_url VITE_API_BASE_URL
    require_frontend_runtime_url VITE_TELEMETRY_ENDPOINT
    require_cors_origins KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON

    validate_compose_config

    if [ "$ERRORS" -eq 0 ]; then
        log_success "Configuration validation passed"
    fi
}

validate_example_config() {
    log_step "Validating public example configuration"
    load_env_file

    if [ "$ERRORS" -gt 0 ]; then
        return
    fi

    local required_values=(
        POSTGRES_PASSWORD
        REDIS_PASSWORD
        JWT_SECRET
        GATEWAY_ENCRYPTION_KEY
        AI_PLATFORM_INTERNAL_TOKEN
        AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET
        AI_PLATFORM_CAPABILITY_PROOF_SECRET
        AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN
        MODEL_SETUP_MODE
        INTERNAL_COMM_REDIS_URL
        AUTH_ALLOWED_EMAIL_DOMAIN
        DEFAULT_USER_PASSWORD
        KB_SERVICE_URL
        GATEWAY_IMAGE
        FRONTEND_IMAGE
        KNOWLEDGE_IMAGE
        MIGRATE_IMAGE
        KB_EMBEDDING_PROVIDER
        KB_EMBEDDING_MODEL
        KB_EMBEDDING_DIMENSION
        KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON
    )
    local provider_keys=(
        DASHSCOPE_CHAT_API_KEY
        DASHSCOPE_API_KEY
        GOOGLE_API_KEY
        GEMINI_API_KEY
        VERTEX_CHAT_API_KEY
        VERTEX_API_KEY
        OPENAI_API_KEY
        ANTHROPIC_API_KEY
        DEEPSEEK_API_KEY
        KB_EMBEDDING_API_KEY
    )
    local key

    for key in "${required_values[@]}"; do
        require_example_value "$key"
    done
    for key in "${provider_keys[@]}"; do
        require_example_key "$key"
    done

    require_positive_int POSTGRES_PORT 5432
    require_positive_int REDIS_PORT 6379
    require_positive_int QDRANT_HTTP_PORT 6333
    require_positive_int QDRANT_GRPC_PORT 6334
    require_positive_int GATEWAY_PORT 8080
    require_positive_int FRONTEND_PORT 8081
    require_positive_int KNOWLEDGE_SERVICE_PORT 8092
    require_positive_int KB_EMBEDDING_DIMENSION 1024

    require_domain AUTH_ALLOWED_EMAIL_DOMAIN
    require_frontend_auth_domain_alignment
    require_support_email_release_ready
    require_url KB_SERVICE_URL
    require_frontend_runtime_url VITE_API_URL
    require_frontend_runtime_url VITE_API_BASE_URL
    require_frontend_runtime_url VITE_TELEMETRY_ENDPOINT
    require_cors_origins KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON

    local embedding_provider="${KB_EMBEDDING_PROVIDER:-dashscope}"
    case "$embedding_provider" in
        gemini|dashscope|siliconflow)
            ;;
        *)
            fail "KB_EMBEDDING_PROVIDER must be one of: gemini, dashscope, siliconflow."
            ;;
    esac

    for key in GATEWAY_IMAGE FRONTEND_IMAGE KNOWLEDGE_IMAGE MIGRATE_IMAGE AGENT_CAPABILITY_WORKER_IMAGE; do
        require_versioned_image "$key"
    done

    validate_ports
    validate_compose_config

    if [ "$ERRORS" -eq 0 ]; then
        log_success "Example configuration validation passed"
    fi
}

validate_runtime() {
    log_step "Validating runtime dependencies"
    require_docker

    wait_for_healthy "PostgreSQL authenticated query" "check_postgres_health" 60 || fail "PostgreSQL runtime check failed."
    wait_for_healthy "Redis authenticated ping" "check_redis_health" 60 || fail "Redis runtime check failed."
    wait_for_healthy "Qdrant health endpoint" "check_qdrant_health" 60 || fail "Qdrant runtime check failed."

    if [ "$INFRA_ONLY" = true ]; then
        if [ "$ERRORS" -eq 0 ]; then
            log_success "Infrastructure runtime validation passed"
        fi
        return
    fi

    wait_for_healthy "Knowledge service" "check_knowledge_health" 60 || fail "Knowledge service runtime check failed."
    wait_for_healthy "Knowledge worker" "check_knowledge_worker_health" 60 || fail "Knowledge worker runtime check failed."
    wait_for_healthy "Agent Runtime" "check_agent_runtime_health" 60 || fail "Agent Runtime runtime check failed."
    wait_for_healthy "Gateway readiness" "check_gateway_health" 60 || fail "Gateway runtime check failed."
    wait_for_healthy "Gateway metrics endpoint" "check_gateway_metrics" 60 || fail "Gateway metrics check failed."
    wait_for_healthy "Frontend health endpoint" "check_frontend_health" 60 || fail "Frontend runtime check failed."
    validate_chat_model_alignment

    if [ "$ERRORS" -eq 0 ]; then
        log_success "Runtime validation passed"
    fi
}

main() {
    if [ "$EXAMPLE_CHECK" = true ]; then
        validate_example_config
    elif [ "$CONFIG_CHECK" = true ]; then
        validate_config
    fi

    if [ "$RUNTIME_CHECK" = true ] && [ "$ERRORS" -eq 0 ]; then
        validate_runtime
    fi

    if [ "$ERRORS" -gt 0 ]; then
        log_error "Validation failed with $ERRORS error(s)."
        exit 1
    fi

    if [ "$WARNINGS" -gt 0 ]; then
        log_warn "Validation completed with $WARNINGS warning(s)."
    fi
}

main "$@"
