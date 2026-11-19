#!/usr/bin/env bash
# SPDX license admission for an immutable release image; never rewrites policy.
set -euo pipefail
if [[ $# != 3 ]]; then
  echo 'Usage: scan_release_license.sh IMAGE@sha256:DIGEST linux/ARCH OUTPUT_JSON' >&2
  exit 2
fi
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image="$1"
platform="$2"
if ! [[ "$image" =~ ^ghcr\.io/[a-z0-9._/-]+@sha256:[0-9a-f]{64}$ ]]; then exit 2; fi
if [[ "$platform" != linux/amd64 && "$platform" != linux/arm64 ]]; then exit 2; fi
work="$(mktemp -d "${TMPDIR:-/tmp}/agentforge-license.XXXXXX")"
trap 'rm -rf "$work"' EXIT
for variable in "${!TRIVY_@}"; do unset "$variable"; done
trivy image --config "$root/deploy/security/trivy.yaml" --image-src remote \
  --platform "$platform" --scanners license --license-full \
  --ignorefile /dev/null --format spdx-json --output "$work/sbom.json" "$image"
platform-tools dependency-policy spdx --repo-root "$root" --image-ref "$image" \
  --sbom "$work/sbom.json" --evidence-out "$3"
