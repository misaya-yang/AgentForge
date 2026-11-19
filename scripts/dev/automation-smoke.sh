#!/usr/bin/env bash
# An existing acceptance stack and its controlled native tools are required.
set -euo pipefail
set +x
command -v automation-smoke >/dev/null || { echo 'Build automation-smoke with scripts/new/native-container.sh and install the controlled command shim first' >&2; exit 1; }
exec automation-smoke
