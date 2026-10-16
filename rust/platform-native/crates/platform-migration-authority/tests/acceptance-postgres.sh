#!/usr/bin/env bash
# Run against an explicitly provisioned disposable PostgreSQL database only.
# Required: exact distinct AI_GATEWAY_DATABASE_{ADMIN,MIGRATOR,GATEWAY,RUNTIME,
# CAPABILITY_WORKER,KNOWLEDGE_API,KNOWLEDGE_WORKER}_DSN and AI_GATEWAY_DATABASE_DIR.
set -euo pipefail
: "${AI_PLATFORM_ISOLATED_TEST_FIXTURE:?Set to 1 only for a disposable fixture}"
[[ "$AI_PLATFORM_ISOLATED_TEST_FIXTURE" == 1 ]]
: "${AUTHORITY_BIN:?Path to built database-authority}"
: "${PROVISION_FIXTURE_EXTENSIONS_BIN:?Path to built provision-fixture-extensions example}"
"$AUTHORITY_BIN" provision-roles
"$PROVISION_FIXTURE_EXTENSIONS_BIN"
"$AUTHORITY_BIN" verify-role-connections
"$AUTHORITY_BIN" init-fresh
"$AUTHORITY_BIN" verify
"$AUTHORITY_BIN" status
# Re-entry must preserve completed epochs and validate checksums.
"$AUTHORITY_BIN" migrate
"$AUTHORITY_BIN" verify
# The historical startup-check CLI directly probes public schema and has an older
# epoch window; it is deliberately not an application admission acceptance test.
# Native service admission must be verified through each service startup contract.

