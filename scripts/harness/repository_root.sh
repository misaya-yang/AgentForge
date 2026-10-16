#!/usr/bin/env bash
# Source-only helper. Never derive this repository from the caller's cwd.
platform_repository_root() {
    local anchor candidate git_root
    anchor="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)" || return 1
    candidate="$(cd "$anchor/../.." && pwd -P)" || return 1
    if [[ ! -f "$candidate/harness.yml" || ! -f "$candidate/rust/platform-native/Cargo.toml" \
       || ! -f "$candidate/deploy/agent-runtime-source/source-receipt.json" ]]; then
        echo "ERROR: repository markers missing beside harness scripts" >&2
        return 1
    fi
    # Preserve a real checkout identity, but never select a containing unrelated
    # repository when this project was unpacked inside another Git checkout.
    git_root="$(git -C "$candidate" rev-parse --show-toplevel 2>/dev/null)" || git_root=""
    if [[ -n "$git_root" && "$(cd "$git_root" && pwd -P)" == "$candidate" ]]; then
        printf '%s\n' "$git_root"
    else
        printf '%s\n' "$candidate"
    fi
}

# Host loopback proxies are not container loopback proxies. Only forward the
# operator's explicit container settings; otherwise keep Docker's own defaults.
platform_configure_build_proxy_args() {
    platform_build_proxy_args=()
    local proxy_name variable_name proxy_value
    for proxy_name in HTTP_PROXY HTTPS_PROXY NO_PROXY; do
        variable_name="CONTAINER_${proxy_name}"
        proxy_value="${!variable_name:-}"
        if [[ -n "$proxy_value" ]]; then
            platform_build_proxy_args+=(--build-arg "${proxy_name}=${proxy_value}")
        fi
    done
}
