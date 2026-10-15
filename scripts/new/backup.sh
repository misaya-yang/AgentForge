#!/bin/bash
# =============================================================================
# AI Gateway - Database Backup & Restore
# =============================================================================
# Usage:  make backup          / make restore
#   or:   ./scripts/new/backup.sh [OPTIONS]
#
# Options:
#   (default)           Create a new backup
#   --restore [file]    Restore from backup (latest if no file specified)
#   --list              List available backups
#   --backup-dir DIR    Store/list backups in an external directory
#   --env FILE          Use a specific env file instead of .env
# =============================================================================

source "$(dirname "$0")/common.sh"

RESTORE=false
LIST=false
BACKUP_FILE=""
BACKUP_DIR=""
BACKUP_DIR_FROM_CLI=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --restore)
            RESTORE=true; shift
            if [[ $# -gt 0 && ! "$1" =~ ^-- ]]; then
                BACKUP_FILE="$1"; shift
            fi ;;
        --list) LIST=true; shift ;;
        --backup-dir)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--backup-dir requires a directory path"
                exit 2
            fi
            BACKUP_DIR="$2"; BACKUP_DIR_FROM_CLI=true; shift 2 ;;
        --env)
            if [ -z "${2:-}" ] || [[ "${2:-}" =~ ^-- ]]; then
                log_error "--env requires a file path"
                exit 2
            fi
            ENV_FILE="$2"; shift 2 ;;
        -h|--help)
            echo "Usage: $0 [--restore [file]] [--list] [--backup-dir DIR] [--env FILE]"
            exit 0 ;;
        *) log_error "Unknown option: $1"; exit 2 ;;
    esac
done

if [ "$BACKUP_DIR_FROM_CLI" = false ]; then
    # A present env file may select an external directory, while list/help still
    # work on a machine that has not bootstrapped the rest of the stack.
    load_env
    BACKUP_DIR="${AI_PLATFORM_BACKUP_DIR:-}"
fi

if [ -z "$BACKUP_DIR" ]; then
    if [ -n "${XDG_STATE_HOME:-}" ]; then
        BACKUP_DIR="${XDG_STATE_HOME}/ai-gateway/backups"
    elif [ -n "${HOME:-}" ]; then
        BACKUP_DIR="${HOME}/.local/state/ai-gateway/backups"
    else
        log_error "Cannot resolve an external backup directory; set AI_PLATFORM_BACKUP_DIR"
        exit 2
    fi
fi

# Database dumps can contain tenant content and authentication material. Keep
# every generated backup outside the source checkout, even when an operator
# supplies an override containing '..' components or symlinks.
BACKUP_DIR="$(python3 - "$BACKUP_DIR" <<'PY'
from pathlib import Path
import sys

print(Path(sys.argv[1]).expanduser().resolve(strict=False))
PY
)"
case "${BACKUP_DIR}/" in
    "${PROJECT_ROOT}/"*)
        log_error "Backup directory must be outside the source checkout: $PROJECT_ROOT"
        exit 2
        ;;
esac

# -- List backups ------------------------------------------------------------
if [ "$LIST" = true ]; then
    echo "Available backups:"
    ls -lh "$BACKUP_DIR"/*.sql.gz 2>/dev/null || echo "  (none)"
    exit 0
fi

require_env_file
load_env

# -- Restore -----------------------------------------------------------------
if [ "$RESTORE" = true ]; then
    if [ -z "$BACKUP_FILE" ]; then
        BACKUP_FILE=$(ls -t "$BACKUP_DIR"/*.sql.gz 2>/dev/null | head -1)
        if [ -z "$BACKUP_FILE" ]; then
            log_error "No backup files found in $BACKUP_DIR"
            exit 1
        fi
    fi

    [ -f "$BACKUP_FILE" ] || { log_error "File not found: $BACKUP_FILE"; exit 1; }

    log_warn "This will overwrite the current database!"
    confirm "Restore from $(basename "$BACKUP_FILE")?" || { log_info "Cancelled"; exit 0; }

    log_info "Restoring from $(basename "$BACKUP_FILE")..."
    gunzip -c "$BACKUP_FILE" | docker exec -i "$(pg_container)" psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$(pg_database)"
    log_success "Database restored"
    exit 0
fi

# -- Create backup -----------------------------------------------------------
log_step "Creating database backup"

umask 077
mkdir -p "$BACKUP_DIR"

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="${BACKUP_DIR}/gateway_${TIMESTAMP}.sql.gz"

# Keep the pipeline in a conditional so errexit cannot bypass cleanup. With
# pipefail enabled by common.sh, a pg_dump or gzip failure enters this branch.
if ! docker exec "$(pg_container)" pg_dump -U "$(pg_user)" -d "$(pg_database)" | gzip > "$BACKUP_FILE"; then
    rm -f "$BACKUP_FILE"
    log_error "Database backup failed — backup aborted"
    exit 1
fi

BACKUP_SIZE=$(ls -lh "$BACKUP_FILE" | awk '{print $5}')
log_success "Backup created: $BACKUP_FILE ($BACKUP_SIZE)"

# Cleanup: keep last 7
BACKUP_COUNT=$(find "$BACKUP_DIR" -name "*.sql.gz" | wc -l | tr -d ' ')
if [ "$BACKUP_COUNT" -gt 7 ]; then
    log_info "Cleaning up old backups (keeping last 7)..."
    ls -t "$BACKUP_DIR"/*.sql.gz | tail -n +8 | xargs rm -f
fi
