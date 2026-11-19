#!/usr/bin/env bash
set -euo pipefail
exec platform-tools resource-lock "$@"
