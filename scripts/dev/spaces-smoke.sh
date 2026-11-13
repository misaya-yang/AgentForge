#!/usr/bin/env bash
# Walk create-space, create-page, and list against a running knowledge gateway.
# SPACE_SMOKE_BASE_URL is required. The token is read from the environment and
# is never printed.
set -euo pipefail

if [[ -z "${SPACE_SMOKE_BASE_URL:-}" ]]; then
  echo "SPACE_SMOKE_BASE_URL is unset; spaces smoke requires a running gateway" >&2
  exit 1
fi

base="${SPACE_SMOKE_BASE_URL%/}"
auth=()
if [[ -n "${SPACE_SMOKE_AUTHORIZATION:-}" ]]; then
  auth=(-H "Authorization: ${SPACE_SMOKE_AUTHORIZATION}")
fi

space_json="$(curl -fsS "${auth[@]}" -H 'content-type: application/json' \
  -d '{"name":"smoke-space","visibility":"private","kind":"personal"}' \
  "${base}/api/v1/knowledge/spaces")"
space_id="$(printf '%s' "${space_json}" | sed -n 's/.*"space_id":"\([^"]*\)".*/\1/p')"
test -n "${space_id}"

curl -fsS "${auth[@]}" -H 'content-type: application/json' \
  -d '{"title":"smoke-page","kind":"page","content_format":"markdown","source":"native"}' \
  "${base}/api/v1/knowledge/spaces/${space_id}/items" >/dev/null

curl -fsS "${auth[@]}" "${base}/api/v1/knowledge/space-items?view=all&space_id=${space_id}" \
  | grep -q 'smoke-page'

echo "spaces smoke ok"
