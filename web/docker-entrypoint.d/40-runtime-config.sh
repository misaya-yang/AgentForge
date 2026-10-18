#!/bin/sh
set -eu

OUTPUT_PATH="${RUNTIME_CONFIG_OUTPUT_PATH:-/tmp/runtime-config.json}"
AUTH_EMAIL_DOMAIN="${VITE_AUTH_EMAIL_DOMAIN:-example.com}"
SUPPORT_EMAIL="${VITE_SUPPORT_EMAIL:-admin@$AUTH_EMAIL_DOMAIN}"

json_escape() {
  printf '%s' "${1:-}" | od -An -v -tu1 | LC_ALL=C awk '{
    for (i = 1; i <= NF; i++) {
      if ($i == 34) printf "\\\"";
      else if ($i == 92) printf "\\\\";
      else if ($i < 32) printf "\\u%04x", $i;
      else printf "%c", $i;
    }
  }'
}

# This is data, never executable JavaScript. Only public browser settings belong here.
cat >"$OUTPUT_PATH" <<EOF
{
  "apiUrl": "$(json_escape "${VITE_API_URL:-}")",
  "apiBaseUrl": "$(json_escape "${VITE_API_BASE_URL:-}")",
  "authEmailDomain": "$(json_escape "$AUTH_EMAIL_DOMAIN")",
  "supportEmail": "$(json_escape "$SUPPORT_EMAIL")",
  "telemetryEndpoint": "$(json_escape "${VITE_TELEMETRY_ENDPOINT:-}")",
  "sseDebug": "$(json_escape "${VITE_SSE_DEBUG:-}")",
  "agentStudioEnabled": "$(json_escape "${VITE_AGENT_STUDIO_ENABLED:-true}")"
}
EOF
