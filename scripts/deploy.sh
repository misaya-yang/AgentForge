#!/usr/bin/env bash
# Generic local Docker Compose deployment entrypoint.
#
# Usage:
#   scripts/deploy.sh
#   scripts/deploy.sh --build
#   scripts/deploy.sh --pull

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ENV_FILE:-$ROOT_DIR/.env}"
BUILD=false
PULL=false

usage() {
  cat <<'EOF'
Usage: scripts/deploy.sh [--build] [--pull]

Options:
  --build   Rebuild application images before starting services.
  --pull    Pull base/service images before starting services.
  -h, --help

This script expects a local .env file. Start from .env.example and run:
  make validate-config
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --build)
      BUILD=true
      shift
      ;;
    --pull)
      PULL=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ ! -f "$ENV_FILE" ]; then
  echo "Missing env file: $ENV_FILE" >&2
  echo "Copy .env.example to .env and fill required values first." >&2
  exit 1
fi

cd "$ROOT_DIR"

make validate-config

compose=(docker compose --env-file "$ENV_FILE")

if [ "$PULL" = true ]; then
  "${compose[@]}" pull
fi

if [ "$BUILD" = true ]; then
  "${compose[@]}" up -d --build --remove-orphans
else
  "${compose[@]}" up -d --remove-orphans
fi

make validate
