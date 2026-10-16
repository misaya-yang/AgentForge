#!/bin/bash
# =============================================================================
# Native Rust wrapper for the single PostgreSQL migration authority.
# =============================================================================
# This script intentionally contains no psql, SQL-file, legacy-ledger, or lock
# implementation. database-authority owns discovery, reconciliation, locking,
# transactions, attempts, fingerprints and every schema/ledger write.
#
# Usage:  make migrate
#   or:   ./scripts/new/migrate.sh [OPTIONS]
#
# Options:
#   --init       Initialize an empty database from the frozen baseline
#   --status     Show read-only authority status
#   --auto       Classify the source and run required administrator setup
#   --env FILE   Use a specific env file instead of .env
# =============================================================================

source "$(dirname "$0")/common.sh"

AUTHORITY_COMMAND="migrate"
EXPLICIT_ENV_FILE=false
AUTO_MODE=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --init)
            AUTHORITY_COMMAND="init-fresh"
            shift
            ;;
        --status)
            AUTHORITY_COMMAND="status"
            shift
            ;;
        --auto)
            AUTO_MODE=true
            shift
            ;;
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            EXPLICIT_ENV_FILE=true
            ENV_FILE="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--init] [--status] [--auto] [--env FILE]"
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            exit 2
            ;;
    esac
done

if [ "$EXPLICIT_ENV_FILE" = true ]; then
    require_env_file
fi
load_env
export AI_GATEWAY_AUTHORITY_SOURCE_MODE="${AI_GATEWAY_AUTHORITY_SOURCE_MODE:-distribution}"
cd "$PROJECT_ROOT"

if ! command -v database-authority >/dev/null 2>&1; then
    log_error "database-authority is required; build/install the native Rust tools first"
    exit 1
fi

uri_component() {
    local LC_ALL=C value="$1" output="" character hex index
    for ((index = 0; index < ${#value}; index++)); do
        character="${value:index:1}"
        case "$character" in
            [a-zA-Z0-9.~_-]) output+="$character" ;;
            *) printf -v hex '%%%02X' "'$character"; output+="$hex" ;;
        esac
    done
    printf '%s' "$output"
}

require_migrator_dsn() {
    if [ -n "${AI_GATEWAY_DATABASE_MIGRATOR_DSN:-}" ]; then
        return 0
    fi
    if [ -z "${LOCAL_DB_MIGRATOR_PASSWORD:-}" ]; then
        log_error "Set AI_GATEWAY_DATABASE_MIGRATOR_DSN or LOCAL_DB_MIGRATOR_PASSWORD"
        return 1
    fi

    local prefix="${AI_GATEWAY_ROLE_PREFIX:-ai_gateway_}"
    local pg_host="${POSTGRES_HOST:-127.0.0.1}"
    local pg_port="${POSTGRES_PORT:-5432}"
    local pg_database="${POSTGRES_DB:-gateway}"
    local url_host encoded_user encoded_password encoded_database

    if [[ ! "$prefix" =~ ^[a-z][a-z0-9_]{0,20}_$ ]]; then
        log_error "AI_GATEWAY_ROLE_PREFIX is invalid"
        return 1
    fi
    if [[ "$pg_host" == *:* ]]; then
        if [[ ! "$pg_host" =~ ^[0-9A-Fa-f:.]+$ ]]; then
            log_error "POSTGRES_HOST is invalid for a local migrator DSN"
            return 1
        fi
        url_host="[$pg_host]"
    else
        if [[ ! "$pg_host" =~ ^[A-Za-z0-9.-]+$ ]]; then
            log_error "POSTGRES_HOST is invalid for a local migrator DSN"
            return 1
        fi
        url_host="$pg_host"
    fi
    if [[ ! "$pg_port" =~ ^[0-9]{1,5}$ ]] \
        || (( 10#$pg_port < 1 || 10#$pg_port > 65535 )); then
        log_error "POSTGRES_PORT is invalid"
        return 1
    fi

    encoded_user="$(uri_component "${prefix}migrator")"
    encoded_password="$(uri_component "$LOCAL_DB_MIGRATOR_PASSWORD")"
    encoded_database="$(uri_component "$pg_database")"
    AI_GATEWAY_DATABASE_MIGRATOR_DSN="postgresql://${encoded_user}:${encoded_password}@${url_host}:${pg_port}/${encoded_database}"
    export AI_GATEWAY_DATABASE_MIGRATOR_DSN
}

require_distinct_admin_dsn() {
    if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
        log_error "AI_GATEWAY_DATABASE_ADMIN_DSN is required for administrator bootstrap"
        return 1
    fi
    if [ "$AI_GATEWAY_DATABASE_ADMIN_DSN" = "$AI_GATEWAY_DATABASE_MIGRATOR_DSN" ]; then
        log_error "AI_GATEWAY_DATABASE_ADMIN_DSN must be distinct from AI_GATEWAY_DATABASE_MIGRATOR_DSN"
        return 1
    fi
    return 0
}

require_migrator_dsn || exit 1

log_step "Database authority: ${AUTHORITY_COMMAND}"

# Fresh-install initialization still uses the native source classifier. It
# pre-provisions roles first so the dedicated migrator can classify the empty
# database, then has the administrator install required extensions.
if [ "$AUTHORITY_COMMAND" = "init-fresh" ]; then
    require_distinct_admin_dsn || exit 1
    database-authority provision-roles
    source_kind="$(database-authority source-kind)"
    if [ "$source_kind" = "empty" ]; then
        database-authority provision-extensions
    fi
    exec database-authority init-fresh
fi

# Classify the source read-only, then preserve historical cutover ordering:
# tracked legacy sources complete their immutable chain before admin ownership
# cutover; ledgerless platform schemas use explicit cutover and adoption.
if [ "$AUTO_MODE" = true ] && [ "$AUTHORITY_COMMAND" = "migrate" ]; then
    if [ -n "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
        require_distinct_admin_dsn || exit 1
        database-authority provision-roles
    fi
    source_kind="$(database-authority source-kind)"
    case "$source_kind" in
        empty)
            if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
                log_error "AI_GATEWAY_DATABASE_ADMIN_DSN is required to bootstrap an empty database"
                exit 1
            fi
            database-authority provision-extensions
            exec database-authority migrate
            ;;
        adopted)
            exec database-authority migrate
            ;;
        ledgerless-platform)
            if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
                exec database-authority migrate
            fi
            database-authority \
                prepare-cutover-ownership \
                --expected-database "${POSTGRES_DB:-gateway}"
            exec database-authority migrate
            ;;
        tracked-legacy)
            if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
                exec database-authority migrate
            fi
            database-authority migrate --no-adoption
            database-authority \
                prepare-cutover-ownership \
                --expected-database "${POSTGRES_DB:-gateway}"
            exec database-authority migrate
            ;;
        *)
            log_error "Database authority returned unknown source kind"
            exit 1
            ;;
    esac
fi

exec database-authority "$AUTHORITY_COMMAND"
