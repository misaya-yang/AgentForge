#!/bin/sh
set -eu

OUTPUT_PATH="${RUNTIME_CONFIG_OUTPUT_PATH:-/usr/share/nginx/html/runtime-config.js}"
AUTH_EMAIL_DOMAIN="${VITE_AUTH_EMAIL_DOMAIN:-example.com}"
SUPPORT_EMAIL="${VITE_SUPPORT_EMAIL:-admin@$AUTH_EMAIL_DOMAIN}"

js_escape() {
  printf '%s' "${1:-}" | tr '\r\n' '  ' | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# Merge onto any config the page already set rather than replacing it, matching
# the committed public/runtime-config.js template. A hard assignment silently
# discarded config injected before this script runs.
cat >"$OUTPUT_PATH" <<EOF
window.__AI_GATEWAY_RUNTIME_CONFIG__ = Object.assign({
  apiUrl: "$(js_escape "${VITE_API_URL:-}")",
  apiBaseUrl: "$(js_escape "${VITE_API_BASE_URL:-}")",
  authEmailDomain: "$(js_escape "$AUTH_EMAIL_DOMAIN")",
  supportEmail: "$(js_escape "$SUPPORT_EMAIL")",
  telemetryEndpoint: "$(js_escape "${VITE_TELEMETRY_ENDPOINT:-}")",
  sseDebug: "$(js_escape "${VITE_SSE_DEBUG:-}")",
  agentStudioEnabled: "$(js_escape "${VITE_AGENT_STUDIO_ENABLED:-true}")",
}, window.__AI_GATEWAY_RUNTIME_CONFIG__ || {});
EOF
