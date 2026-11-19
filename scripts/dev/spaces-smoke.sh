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

if [[ "${1:-}" == "--rag" ]]; then
  phrase="space-rag-$(date +%s)-$$"
  item_json="$(curl -fsS "${auth[@]}" -H 'content-type: application/json' \
    -d "{\"title\":\"rag-${phrase}\",\"kind\":\"page\",\"content_format\":\"markdown\",\"source\":\"native\"}" \
    "${base}/api/v1/knowledge/spaces/${space_id}/items")"
  item_id="$(printf '%s' "${item_json}" | sed -n 's/.*"item_id":"\([^"]*\)".*/\1/p')"
  dataset_id="$(printf '%s' "${space_json}" | sed -n 's/.*"dataset_id":"\([^"]*\)".*/\1/p')"
  test -n "${item_id}"
  test -n "${dataset_id}"
  curl -fsS "${auth[@]}" -H 'content-type: application/json' -H 'If-Match: "0"' \
    -X PUT \
    -d "{\"content_format\":\"markdown\",\"blocks\":null,\"markdown\":\"${phrase}\"}" \
    "${base}/api/v1/knowledge/space-items/${item_id}/content" >/dev/null
  found=""
  for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    sleep 2
    retrieved="$(curl -fsS "${auth[@]}" -H 'content-type: application/json' \
      -d "{\"query\":\"${phrase}\",\"top_k\":5}" \
      "${base}/api/v1/knowledge/${dataset_id}/retrieve" || true)"
    if printf '%s' "${retrieved}" | grep -q "${phrase}" && printf '%s' "${retrieved}" | grep -q "${item_id}"; then
      found="${retrieved}"
      break
    fi
  done
  test -n "${found}"
  denied_code="$(curl -sS -o /dev/null -w '%{http_code}' -H 'content-type: application/json' \
    -d "{\"query\":\"${phrase}\",\"top_k\":5}" \
    "${base}/api/v1/knowledge/${dataset_id}/retrieve" || true)"
  test "${denied_code}" != "200"
  echo "spaces rag smoke ok"
fi

echo "spaces smoke ok"
