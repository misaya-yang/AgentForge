#!/usr/bin/env bash
# Produce a critical-scan predicate for an immutable image; no signing or deployment.
set -euo pipefail
if [[ $# != 3 ]]; then
  echo 'Usage: scan_release_image.sh IMAGE@sha256:DIGEST linux/ARCH OUTPUT_JSON' >&2
  exit 2
fi
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image="$1"
platform="$2"
output="$3"
if ! [[ "$image" =~ ^ghcr\.io/[a-z0-9._/-]+@sha256:[0-9a-f]{64}$ ]]; then
  echo 'Release scan requires an immutable GHCR SHA256 image reference' >&2
  exit 2
fi
if [[ "$platform" != linux/amd64 && "$platform" != linux/arm64 ]]; then
  echo 'Release scan requires linux/amd64 or linux/arm64' >&2
  exit 2
fi
work="$(mktemp -d "${TMPDIR:-/tmp}/agentforge-scan.XXXXXX")"
trap 'rm -rf "$work"' EXIT
# Prevent inherited TRIVY_* settings from overriding this release policy. Registry
# authentication still uses the existing Docker credential configuration.
for variable in "${!TRIVY_@}"; do unset "$variable"; done
# Full vulnerability results, including unfixed findings. No local ignore/VEX bypass.
trivy image --config "$root/deploy/security/trivy.yaml" --image-src remote \
  --platform "$platform" --scanners vuln --severity UNKNOWN,LOW,MEDIUM,HIGH,CRITICAL \
  --ignore-unfixed=false --ignorefile /dev/null --skip-db-update=false \
  --format json --exit-code 0 --output "$work/report.json" "$image"
trivy --version --format json > "$work/database.json"
platform-tools supply-chain scan-report --repo-root "$root" --image-ref "$image" \
  --platform "$platform" --report "$work/report.json" \
  --scanner-metadata "$work/database.json" --evidence-out "$output"
