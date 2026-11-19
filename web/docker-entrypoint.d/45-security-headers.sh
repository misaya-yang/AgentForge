#!/bin/sh
set -eu

OUTPUT_PATH="${FRONTEND_SECURITY_HEADERS_OUTPUT_PATH:-/tmp/frontend-security-headers.conf}"

# Only origin tokens are allowed: no paths, wildcards, quotes or nginx/CSP directives.
origins() {
  printf '%s' "$1" | LC_ALL=C awk -v websocket="$2" '
    {
      for (i = 1; i <= NF; i++) {
        value = $i;
        if (value !~ /^https?:\/\/(\[[0-9a-fA-F:.]+\]|[A-Za-z0-9.-]+)(:[0-9]+)?$/ &&
            !(websocket && value ~ /^wss?:\/\/(\[[0-9a-fA-F:.]+\]|[A-Za-z0-9.-]+)(:[0-9]+)?$/)) exit 1;
        port = value;
        if (sub(/^.*\]:/, "", port) || (port !~ /\[/ && sub(/^.*:/, "", port) && port ~ /^[0-9]+$/)) {
          if (port + 0 < 1 || port + 0 > 65535) exit 1;
        }
        printf " %s", value;
      }
    }'
}
CONNECT_ORIGINS=$(origins "${FRONTEND_CONNECT_ORIGINS:-}" 1) || {
  echo "Invalid FRONTEND_CONNECT_ORIGINS: expected explicit HTTP(S)/WS(S) origins" >&2
  exit 1
}
IMAGE_ORIGINS=$(origins "${FRONTEND_IMAGE_ORIGINS:-}" 0) || {
  echo "Invalid FRONTEND_IMAGE_ORIGINS: expected explicit HTTP(S) origins" >&2
  exit 1
}

BASE="default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self' data:; object-src 'none'; base-uri 'self'; frame-ancestors 'self'; form-action 'self'"
STRICT="$BASE; img-src 'self' data: blob:$IMAGE_ORIGINS; connect-src 'self'$CONNECT_ORIGINS; require-trusted-types-for 'script'; trusted-types dompurify agentforge-entities"
case "${FRONTEND_CSP_MODE:-report-only}" in
  report-only)
    # Preserve the current policy for one release while measuring the strict candidate.
    POLICY="$BASE; img-src 'self' data: blob: https:; connect-src 'self' https: wss:; upgrade-insecure-requests"
    REPORT="add_header Content-Security-Policy-Report-Only \"$STRICT\" always;"
    ;;
  enforce) POLICY="$STRICT"; REPORT="" ;;
  *) echo "Invalid FRONTEND_CSP_MODE: expected report-only or enforce" >&2; exit 1 ;;
esac

cat >"$OUTPUT_PATH" <<EOF
add_header X-Frame-Options "SAMEORIGIN" always;
add_header X-Content-Type-Options "nosniff" always;
add_header Referrer-Policy "strict-origin-when-cross-origin" always;
add_header Permissions-Policy "camera=(self), microphone=(self), geolocation=(), payment=(), usb=()" always;
add_header Cross-Origin-Opener-Policy "same-origin" always;
add_header Cross-Origin-Resource-Policy "same-origin" always;
add_header Content-Security-Policy "$POLICY" always;
$REPORT
EOF
