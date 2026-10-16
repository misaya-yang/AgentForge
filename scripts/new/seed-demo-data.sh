#!/bin/bash
# =============================================================================
# AI Gateway - Local Demo Data Seeder
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

SQL_FILE="${PROJECT_ROOT}/examples/demo-data/open-source-demo.sql"
APPLY=false

usage() {
    cat <<'USAGE'
Usage: scripts/new/seed-demo-data.sh [--env FILE] [--sql FILE] [--dry-run] [--apply]

Loads deterministic demo records for local open-source evaluation.

Default mode is --dry-run. It prints the SQL path, routes, and a preview without
connecting to PostgreSQL. Use --apply only against a local development database.

Options:
  --env FILE   Env file used when applying SQL (default: .env)
  --sql FILE   SQL seed file to preview or apply
  --dry-run    Preview only; no database writes (default)
  --apply      Apply the SQL to the configured local PostgreSQL database
  -h, --help   Show this help
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
        --sql)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--sql requires a file path"
                exit 2
            fi
            SQL_FILE="$2"
            shift 2
            ;;
        --dry-run)
            APPLY=false
            shift
            ;;
        --apply)
            APPLY=true
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

if [ ! -r "$SQL_FILE" ]; then
    log_error "Demo seed SQL not found: $SQL_FILE"
    exit 1
fi

print_routes() {
    cat <<'ROUTES'
Demo routes after applying to a local stack:
  /knowledge/demo-kb-ai-gateway
  /share/demo-share
  /quiz/demo-quiz
ROUTES
}

if [ "$APPLY" != "true" ]; then
    log_step "Demo data dry run"
    log_info "SQL file: $SQL_FILE"
    log_info "No database connection will be opened in dry-run mode."
    echo ""
    print_routes
    echo ""
    log_info "Preview:"
    sed -n '1,80p' "$SQL_FILE"
    echo ""
    log_success "Dry run complete. Re-run with --apply to write to the configured local database."
    exit 0
fi

log_step "Apply demo data"
log_warn "This writes deterministic demo records to the database configured by $(env_file_path)."
require_env_file
load_env

run_sql_file "$SQL_FILE"

echo ""
print_routes
log_success "Demo data applied."
