#!/bin/bash
# =============================================================================
# Compatibility wrapper for the single PostgreSQL migration authority.
# =============================================================================
# This script intentionally contains no psql, SQL-file, legacy-ledger, or lock
# implementation. ``database.authority`` owns discovery, reconciliation,
# locking, transactions, attempts, fingerprints and every schema/ledger write.
#
# Usage:  make migrate
#   or:   ./scripts/new/migrate.sh [OPTIONS]
#
# Options:
#   --init       Initialize an empty database from the frozen baseline
#   --status     Show read-only authority status
#   --auto       Accepted for deploy compatibility; migration stays fail-fast
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
cd "$PROJECT_ROOT"

if ! command -v uv >/dev/null 2>&1; then
    log_error "uv is required to run the repository-pinned database authority"
    exit 1
fi

# Local Compose-era env files predate the role-specific authority variable.
# Keep the authority itself fail-closed while adapting that explicit local
# bootstrap credential at this compatibility boundary. Managed deployments
# set AI_GATEWAY_DATABASE_MIGRATOR_DSN directly and never enter this branch.
if [ -z "${AI_GATEWAY_DATABASE_MIGRATOR_DSN:-}" ] && [ -n "${POSTGRES_PASSWORD:-}" ]; then
    local_authority_dsn="$({
        python3 - "${POSTGRES_HOST:-127.0.0.1}" "${POSTGRES_PORT:-5432}" \
            "${POSTGRES_USER:-postgres}" "$POSTGRES_PASSWORD" \
            "${POSTGRES_DB:-gateway}" <<'PY'
import sys
from urllib.parse import quote

host, port, user, password, database = sys.argv[1:]
print(
    f"postgresql://{quote(user, safe='')}:{quote(password, safe='')}@"
    f"{host}:{port}/{quote(database, safe='')}"
)
PY
    })"
    AI_GATEWAY_DATABASE_MIGRATOR_DSN="$local_authority_dsn"
    export AI_GATEWAY_DATABASE_MIGRATOR_DSN
    if [ -z "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
        AI_GATEWAY_DATABASE_ADMIN_DSN="$local_authority_dsn"
        export AI_GATEWAY_DATABASE_ADMIN_DSN
    fi
fi

log_step "Database authority: ${AUTHORITY_COMMAND}"

# Local deploy owns the explicit admin bootstrap phase required by ARC-03.
# Classify the source read-only, then route it without guessing migration
# history. Tracked legacy sources complete their immutable chain before admin
# ownership cutover. Ledgerless platform schemas go directly through explicit
# admin cutover and four-fingerprint adoption; empty/adopted sources need only
# the normal authority command.
if [ "$AUTO_MODE" = true ] && [ "$AUTHORITY_COMMAND" = "migrate" ] \
    && [ -n "${AI_GATEWAY_DATABASE_ADMIN_DSN:-}" ]; then
    uv run --extra database python -m database.authority provision-roles
    source_kind="$(uv run --extra database python -m database.authority source-kind)"
    case "$source_kind" in
        empty|adopted)
            exec uv run --extra database python -m database.authority migrate
            ;;
        ledgerless-platform)
            uv run --extra database python -m database.authority \
                prepare-cutover-ownership \
                --expected-database "${POSTGRES_DB:-gateway}"
            exec uv run --extra database python -m database.authority migrate
            ;;
        tracked-legacy)
            uv run --extra database python -m database.authority migrate --no-adoption
            uv run --extra database python -m database.authority \
                prepare-cutover-ownership \
                --expected-database "${POSTGRES_DB:-gateway}"
            exec uv run --extra database python -m database.authority migrate
            ;;
        *)
            log_error "Database authority returned unknown source kind"
            exit 1
            ;;
    esac
fi

exec uv run --extra database python -m database.authority "$AUTHORITY_COMMAND"
