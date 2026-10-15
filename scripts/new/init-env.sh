#!/bin/bash
# =============================================================================
# AI Gateway - Local Env Initializer
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/common.sh"

ENV_FILE="${ENV_FILE:-${PROJECT_ROOT}/.env}"
FORCE=false
IF_MISSING=false

usage() {
    cat <<'USAGE'
Usage: scripts/new/init-env.sh [--env FILE] [--force] [--if-missing]

Creates a local env file from .env.example without printing secret values.

Options:
  --env FILE     Destination env file. Defaults to .env
  --force        Replace an existing env file
  --if-missing   Exit successfully when the env file already exists
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
        --force)
            FORCE=true
            shift
            ;;
        --if-missing)
            IF_MISSING=true
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

EXAMPLE_FILE="${PROJECT_ROOT}/.env.example"

generate_secret() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
        return
    fi

    dd if=/dev/urandom bs=32 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
}

if [ ! -r "$EXAMPLE_FILE" ]; then
    log_error "Example env file not found: $EXAMPLE_FILE"
    exit 1
fi

if [ -e "$ENV_FILE" ] && [ "$FORCE" != true ]; then
    if [ "$IF_MISSING" = true ]; then
        upgrade_file="$(mktemp "${ENV_FILE}.upgrade.XXXXXX")"
        trap 'rm -f "$upgrade_file"' EXIT
        cp "$ENV_FILE" "$upgrade_file"
        upgraded=false
        for generated_key in GATEWAY_ENCRYPTION_KEY AI_PLATFORM_INTERNAL_TOKEN AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET AI_PLATFORM_CAPABILITY_PROOF_SECRET AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN; do
            if awk -F= -v key="$generated_key" '$1 == key && length($0) > length($1) + 1 {found=1} END {exit !found}' "$upgrade_file"; then
                continue
            fi
            generated_value="${!generated_key:-}"
            if [ -z "$generated_value" ]; then
                generated_value="$(generate_secret)"
            fi
            awk -F= -v key="$generated_key" '$1 != key {print}' "$upgrade_file" > "${upgrade_file}.next"
            printf '%s=%s\n' "$generated_key" "$generated_value" >> "${upgrade_file}.next"
            mv "${upgrade_file}.next" "$upgrade_file"
            upgraded=true
        done
        kernel_revision="$(agent_runtime_kernel_revision)"
        existing_kernel_revision="$(awk -F= '$1 == "AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION" {print substr($0, length($1) + 2); exit}' "$upgrade_file")"
        if [ -z "$existing_kernel_revision" ] \
            || { [[ "$existing_kernel_revision" =~ ^[0-9a-f]{40}\+[0-9a-f]{12}$ ]] \
                && [ "$existing_kernel_revision" != "$kernel_revision" ]; }; then
            awk -F= '$1 != "AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION" {print}' "$upgrade_file" > "${upgrade_file}.next"
            printf 'AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION=%s\n' "$kernel_revision" >> "${upgrade_file}.next"
            mv "${upgrade_file}.next" "$upgrade_file"
            upgraded=true
        fi
        runtime_image="$(agent_runtime_image_tag)"
        existing_runtime_image="$(awk -F= '$1 == "AI_PLATFORM_AGENT_RUNTIME_IMAGE" {print substr($0, length($1) + 2); exit}' "$upgrade_file")"
        if [ -z "$existing_runtime_image" ] \
            || { [[ "$existing_runtime_image" == ai-gateway-agent-runtime:local-* ]] \
                && [ "$existing_runtime_image" != "$runtime_image" ]; }; then
            awk -F= '$1 != "AI_PLATFORM_AGENT_RUNTIME_IMAGE" {print}' "$upgrade_file" > "${upgrade_file}.next"
            printf 'AI_PLATFORM_AGENT_RUNTIME_IMAGE=%s\n' "$runtime_image" >> "${upgrade_file}.next"
            mv "${upgrade_file}.next" "$upgrade_file"
            upgraded=true
        fi
        worker_image="$(agent_capability_worker_image_tag)"
        existing_worker_image="$(awk -F= '$1 == "AGENT_CAPABILITY_WORKER_IMAGE" {print substr($0, length($1) + 2); exit}' "$upgrade_file")"
        if [ -z "$existing_worker_image" ] \
            || { [[ "$existing_worker_image" == ai-gateway-agent-capability-worker:local-* ]] \
                && [ "$existing_worker_image" != "$worker_image" ]; }; then
            awk -F= '$1 != "AGENT_CAPABILITY_WORKER_IMAGE" {print}' "$upgrade_file" > "${upgrade_file}.next"
            printf 'AGENT_CAPABILITY_WORKER_IMAGE=%s\n' "$worker_image" >> "${upgrade_file}.next"
            mv "${upgrade_file}.next" "$upgrade_file"
            upgraded=true
        fi
        if [ "$upgraded" = true ]; then
            chmod 600 "$upgrade_file"
            mv "$upgrade_file" "$ENV_FILE"
            trap - EXIT
            log_success "Added missing generated configuration to: $ENV_FILE"
        fi
        log_success "Env file already exists: $ENV_FILE"
        exit 0
    fi
    log_error "Env file already exists: $ENV_FILE. Use --force to replace it."
    exit 1
fi

env_value() {
    local key="$1"
    eval "printf '%s' \"\${${key}:-}\""
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

copy_env_or_default() {
    local key="$1"
    local default_value="$2"
    local value
    value="$(env_value "$key")"

    if [ -n "$value" ]; then
        printf '%s' "$value"
        return
    fi

    printf '%s' "$default_value"
}

POSTGRES_PASSWORD_VALUE="$(copy_env_or_default POSTGRES_PASSWORD "$(generate_secret)")"
REDIS_PASSWORD_VALUE="$(copy_env_or_default REDIS_PASSWORD "$(generate_secret)")"
JWT_SECRET_VALUE="$(copy_env_or_default JWT_SECRET "$(generate_secret)")"
GATEWAY_ENCRYPTION_KEY_VALUE="$(copy_env_or_default GATEWAY_ENCRYPTION_KEY "$(generate_secret)")"
AI_PLATFORM_INTERNAL_TOKEN_VALUE="$(copy_env_or_default AI_PLATFORM_INTERNAL_TOKEN "$(generate_secret)")"
AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET_VALUE="$(copy_env_or_default AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET "$(generate_secret)")"
AI_PLATFORM_CAPABILITY_PROOF_SECRET_VALUE="$(copy_env_or_default AI_PLATFORM_CAPABILITY_PROOF_SECRET "$(generate_secret)")"
AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN_VALUE="$(copy_env_or_default AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN "$(generate_secret)")"
AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION_VALUE="$(copy_env_or_default AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION "$(agent_runtime_kernel_revision)")"
AI_PLATFORM_AGENT_RUNTIME_IMAGE_VALUE="$(copy_env_or_default AI_PLATFORM_AGENT_RUNTIME_IMAGE "$(agent_runtime_image_tag)")"
DEFAULT_USER_PASSWORD_VALUE="$(copy_env_or_default DEFAULT_USER_PASSWORD "$(generate_secret)")"

choose_embedding_provider() {
    local configured
    configured="$(env_value KB_EMBEDDING_PROVIDER)"
    if [ -n "$configured" ]; then
        printf '%s' "$configured"
    elif [ -n "$(env_value GEMINI_API_KEY)" ] || [ -n "$(env_value GOOGLE_API_KEY)" ]; then
        printf 'gemini'
    elif [ -n "$(env_value DASHSCOPE_EMBEDDING_API_KEY)" ] || [ -n "$(env_value DASHSCOPE_API_KEY)" ]; then
        printf 'dashscope'
    elif [ -n "$(env_value SILICONFLOW_API_KEY)" ]; then
        printf 'siliconflow'
    else
        printf 'dashscope'
    fi
}

EMBEDDING_PROVIDER_VALUE="$(choose_embedding_provider)"

choose_embedding_key() {
    local configured
    configured="$(env_value KB_EMBEDDING_API_KEY)"
    if [ -n "$configured" ]; then
        printf '%s' "$configured"
        return
    fi

    # The default provider-specific key is injected separately. Keeping this
    # field empty avoids duplicating the same model secret in the generated
    # file while still allowing a dedicated embedding credential override.
    printf ''
}

KB_EMBEDDING_API_KEY_VALUE="$(choose_embedding_key)"

value_for_key() {
    local key="$1"
    local current="$2"

    case "$key" in
        POSTGRES_PASSWORD)
            printf '%s' "$POSTGRES_PASSWORD_VALUE"
            ;;
        REDIS_PASSWORD)
            printf '%s' "$REDIS_PASSWORD_VALUE"
            ;;
        JWT_SECRET)
            printf '%s' "$JWT_SECRET_VALUE"
            ;;
        GATEWAY_ENCRYPTION_KEY)
            printf '%s' "$GATEWAY_ENCRYPTION_KEY_VALUE"
            ;;
        AI_PLATFORM_INTERNAL_TOKEN)
            printf '%s' "$AI_PLATFORM_INTERNAL_TOKEN_VALUE"
            ;;
        AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET)
            printf '%s' "$AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET_VALUE"
            ;;
        AI_PLATFORM_CAPABILITY_PROOF_SECRET)
            printf '%s' "$AI_PLATFORM_CAPABILITY_PROOF_SECRET_VALUE"
            ;;
        AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN)
            printf '%s' "$AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN_VALUE"
            ;;
        AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION)
            printf '%s' "$AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION_VALUE"
            ;;
        AI_PLATFORM_AGENT_RUNTIME_IMAGE)
            printf '%s' "$AI_PLATFORM_AGENT_RUNTIME_IMAGE_VALUE"
            ;;
        DEFAULT_USER_PASSWORD)
            printf '%s' "$DEFAULT_USER_PASSWORD_VALUE"
            ;;
        INTERNAL_COMM_REDIS_URL)
            printf 'redis://:%s@redis:6379/3' "$REDIS_PASSWORD_VALUE"
            ;;
        KB_EMBEDDING_PROVIDER)
            printf '%s' "$EMBEDDING_PROVIDER_VALUE"
            ;;
        KB_EMBEDDING_API_KEY)
            printf '%s' "$KB_EMBEDDING_API_KEY_VALUE"
            ;;
        GATEWAY_DATABASE_AUTO_INIT|AI_PLATFORM_BACKUP_DIR|DASHSCOPE_API_KEY|DASHSCOPE_OCR_API_KEY|DASHSCOPE_CHAT_API_KEY|DASHSCOPE_IMAGE_API_KEY|DASHSCOPE_EMBEDDING_API_KEY|DASHSCOPE_BASE_URL|DASHSCOPE_OCR_BASE_URL|DASHSCOPE_CHAT_BASE_URL|DASHSCOPE_CHAT_WIRE_PROTOCOL|DASHSCOPE_EMBEDDING_BASE_URL|DASHSCOPE_IMAGE_BASE_URL|DASHSCOPE_IMAGE_MODEL|DASHSCOPE_IMAGE_PROTOCOL|DASHSCOPE_IMAGE_SUBMIT_PATH|DASHSCOPE_RERANK_BASE_URL|DASHSCOPE_RERANK_REQUEST_SCHEMA|DASHSCOPE_RERANK_INSTRUCT|IMAGE_GENERATION_PROVIDER|IMAGE_GENERATION_RESULT_HOST_SUFFIXES|GOOGLE_API_KEY|GEMINI_API_KEY|GOOGLE_API_BACKEND|GOOGLE_CHAT_BACKEND|GOOGLE_IMAGE_BACKEND|GOOGLE_IMAGE_MODEL|VERTEX_API_KEY|VERTEX_CHAT_API_KEY|VERTEX_IMAGE_API_KEY|ARK_API_KEY|ARK_BASE_URL|DOUBAO_IMAGE_MODEL|OPENAI_API_KEY|OPENAI_BASE_URL|OPENAI_WIRE_PROTOCOL|ANTHROPIC_API_KEY|DEEPSEEK_API_KEY|TAVILY_API_KEY|SILICONFLOW_API_KEY|KB_EMBEDDING_MODEL|KB_EMBEDDING_DIMENSION|KB_EMBEDDING_OPENAI_COMPATIBLE_API_KEY|KB_EMBEDDING_OPENAI_COMPATIBLE_BASE_URL|KB_MAX_FILE_SIZE_MB|KB_MAX_BATCH_SIZE_MB|KB_PDF_SPLIT_MAX_SIZE_MB|KB_PDF_SPLIT_PAGES_PER_PART|AUTH_ALLOWED_EMAIL_DOMAIN|GATEWAY_IMAGE|FRONTEND_IMAGE|KNOWLEDGE_IMAGE|MIGRATE_IMAGE|AGENT_CAPABILITY_WORKER_IMAGE|POSTGRES_MEMORY_LIMIT|REDIS_MEMORY_LIMIT|REDIS_MAXMEMORY|QDRANT_MEMORY_LIMIT|GATEWAY_MEMORY_LIMIT|FRONTEND_MEMORY_LIMIT|KNOWLEDGE_MEMORY_LIMIT|GATEWAY_TASK_WORKER_CONCURRENCY|GATEWAY_KNOWLEDGE__WORKER_CONCURRENCY)
            copy_env_or_default "$key" "$current"
            ;;
        *)
            printf '%s' "$current"
            ;;
    esac
}

destination_dir="$(dirname "$ENV_FILE")"
mkdir -p "$destination_dir"
tmp_file="$(mktemp "${ENV_FILE}.tmp.XXXXXX")"
cleanup() {
    rm -f "$tmp_file"
}
trap cleanup EXIT

umask 077
while IFS= read -r line || [ -n "$line" ]; do
    if [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]]; then
        key="${BASH_REMATCH[1]}"
        current="${BASH_REMATCH[2]}"
        printf '%s=%s\n' "$key" "$(value_for_key "$key" "$current")"
    else
        printf '%s\n' "$line"
    fi
done < "$EXAMPLE_FILE" > "$tmp_file"

generated_value() {
    local key="$1"
    awk -F= -v key="$key" '$1 == key {print substr($0, index($0, "=") + 1); exit}' "$tmp_file"
}

upsert_generated_value() {
    local key="$1"
    local value="$2"
    local next_file
    next_file="$(mktemp "${ENV_FILE}.tmp.XXXXXX")"
    awk -F= -v key="$key" '$1 != key {print}' "$tmp_file" > "$next_file"
    printf '%s=%s\n' "$key" "$value" >> "$next_file"
    mv "$next_file" "$tmp_file"
}

POSTGRES_USER_VALUE="$(generated_value POSTGRES_USER)"
POSTGRES_DB_VALUE="$(generated_value POSTGRES_DB)"
POSTGRES_PORT_VALUE="$(generated_value POSTGRES_PORT)"
REDIS_PORT_VALUE="$(generated_value REDIS_PORT)"

upsert_generated_value GATEWAY_DATABASE__ENABLED true
upsert_generated_value GATEWAY_DATABASE__DSN "postgresql://${POSTGRES_USER_VALUE:-postgres}:${POSTGRES_PASSWORD_VALUE}@127.0.0.1:${POSTGRES_PORT_VALUE:-5432}/${POSTGRES_DB_VALUE:-gateway}"
upsert_generated_value GATEWAY_REDIS__ENABLED true
upsert_generated_value GATEWAY_REDIS__URL "redis://:${REDIS_PASSWORD_VALUE}@127.0.0.1:${REDIS_PORT_VALUE:-6379}/0"
upsert_generated_value GATEWAY_AUTHENTICATION__JWT__ENABLED true
upsert_generated_value GATEWAY_AUTHENTICATION__JWT__SECRET "$JWT_SECRET_VALUE"

mv "$tmp_file" "$ENV_FILE"
chmod 600 "$ENV_FILE"
trap - EXIT

log_success "Created env file: $ENV_FILE"
log_info "Generated local secrets for: POSTGRES_PASSWORD REDIS_PASSWORD JWT_SECRET GATEWAY_ENCRYPTION_KEY AI_PLATFORM_INTERNAL_TOKEN AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET AI_PLATFORM_CAPABILITY_PROOF_SECRET AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN DEFAULT_USER_PASSWORD"

COPIED_KEYS=""
for key in POSTGRES_PASSWORD REDIS_PASSWORD JWT_SECRET GATEWAY_ENCRYPTION_KEY AI_PLATFORM_INTERNAL_TOKEN AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET AI_PLATFORM_CAPABILITY_PROOF_SECRET AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN DASHSCOPE_API_KEY DASHSCOPE_OCR_API_KEY DASHSCOPE_CHAT_API_KEY DASHSCOPE_IMAGE_API_KEY DASHSCOPE_EMBEDDING_API_KEY DASHSCOPE_BASE_URL DASHSCOPE_OCR_BASE_URL DASHSCOPE_CHAT_BASE_URL DASHSCOPE_CHAT_WIRE_PROTOCOL DASHSCOPE_EMBEDDING_BASE_URL DASHSCOPE_IMAGE_BASE_URL DASHSCOPE_RERANK_BASE_URL DASHSCOPE_RERANK_REQUEST_SCHEMA DASHSCOPE_RERANK_INSTRUCT GOOGLE_API_KEY GEMINI_API_KEY GOOGLE_API_BACKEND GOOGLE_CHAT_BACKEND VERTEX_API_KEY VERTEX_CHAT_API_KEY OPENAI_API_KEY OPENAI_BASE_URL OPENAI_WIRE_PROTOCOL ANTHROPIC_API_KEY DEEPSEEK_API_KEY TAVILY_API_KEY SILICONFLOW_API_KEY KB_EMBEDDING_PROVIDER KB_EMBEDDING_API_KEY KB_EMBEDDING_MODEL KB_EMBEDDING_DIMENSION AUTH_ALLOWED_EMAIL_DOMAIN DEFAULT_USER_PASSWORD GATEWAY_IMAGE FRONTEND_IMAGE KNOWLEDGE_IMAGE MIGRATE_IMAGE AGENT_CAPABILITY_WORKER_IMAGE; do
    if [ -n "$(env_value "$key")" ]; then
        COPIED_KEYS="${COPIED_KEYS}${COPIED_KEYS:+ }${key}"
    fi
done

if [ -n "$COPIED_KEYS" ]; then
    unique_copied_keys="$(printf '%s\n' $COPIED_KEYS | awk '!seen[$0]++' | xargs)"
    log_info "Copied user-supplied values from the current environment for: $unique_copied_keys"
fi

embedding_key_available=false
case "$EMBEDDING_PROVIDER_VALUE" in
    dashscope)
        if [ -n "$KB_EMBEDDING_API_KEY_VALUE" ] || [ -n "$(env_value DASHSCOPE_EMBEDDING_API_KEY)" ] || [ -n "$(env_value DASHSCOPE_API_KEY)" ]; then
            embedding_key_available=true
        fi
        ;;
    gemini)
        if [ -n "$KB_EMBEDDING_API_KEY_VALUE" ] || [ -n "$(env_value GEMINI_API_KEY)" ] || [ -n "$(env_value GOOGLE_API_KEY)" ]; then
            embedding_key_available=true
        fi
        ;;
    siliconflow)
        if [ -n "$KB_EMBEDDING_API_KEY_VALUE" ] || [ -n "$(env_value SILICONFLOW_API_KEY)" ]; then
            embedding_key_available=true
        fi
        ;;
esac

if [ "$embedding_key_available" != true ]; then
    log_warn "Set the selected model provider key before running full config validation."
fi

if ! grep -Eq '^(DASHSCOPE_CHAT_API_KEY|DASHSCOPE_API_KEY|GOOGLE_API_KEY|GEMINI_API_KEY|VERTEX_API_KEY|VERTEX_CHAT_API_KEY|OPENAI_API_KEY|ANTHROPIC_API_KEY|DEEPSEEK_API_KEY)=[^[:space:]]+' "$ENV_FILE"; then
    log_warn "Set at least one chat model API key before running full config validation."
fi

log_info "Next: make validate-config ENV_FILE=\"$ENV_FILE\""
