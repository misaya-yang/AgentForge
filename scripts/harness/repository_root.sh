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

# Measure available memory inside the actual VM/container hard ceiling. Linux
# /proc/meminfo can describe more RAM than the enclosing cgroup permits.
platform_build_memory_status() {
    local total_bytes available_bytes cgroup_limit cgroup_current
    if [[ -r /proc/meminfo ]]; then
        read -r total_bytes available_bytes < <(awk '
            /^MemTotal:/ {total=$2*1024}
            /^MemAvailable:/ {available=$2*1024}
            END {printf "%.0f %.0f\n",total,available}' /proc/meminfo)
        if [[ -r /sys/fs/cgroup/memory.max && -r /sys/fs/cgroup/memory.current ]]; then
            cgroup_limit="$(</sys/fs/cgroup/memory.max)"
            cgroup_current="$(</sys/fs/cgroup/memory.current)"
            if [[ "$cgroup_limit" =~ ^[1-9][0-9]*$ && "$cgroup_current" =~ ^[0-9]+$ ]]; then
                ((cgroup_limit < total_bytes)) && total_bytes="$cgroup_limit"
                local cgroup_free=$((cgroup_limit > cgroup_current ? cgroup_limit - cgroup_current : 0))
                ((cgroup_free < available_bytes)) && available_bytes="$cgroup_free"
            fi
        fi
        [[ "$total_bytes" =~ ^[1-9][0-9]*$ && "$available_bytes" =~ ^[0-9]+$ ]] || return 1
        printf '%s %s\n' "$total_bytes" "$available_bytes"
        return
    fi
    if command -v memory_pressure >/dev/null 2>&1 && command -v sysctl >/dev/null 2>&1; then
        local free_percent
        total_bytes="$(sysctl -n hw.memsize)" || return 1
        free_percent="$(memory_pressure | awk '/System-wide memory free percentage:/ {gsub(/%/, "", $5); print $5}')"
        [[ "$total_bytes" =~ ^[1-9][0-9]*$ && "$free_percent" =~ ^[0-9]+$ ]] || return 1
        printf '%s %s\n' "$total_bytes" "$((total_bytes * free_percent / 100))"
        return
    fi
    return 1
}

platform_assert_build_headroom() {
    local root="$1" minimum_percent="$2" minimum_disk_mb="$3"
    local measurement total_bytes available_bytes free_percent disk_mb
    measurement="$(platform_build_memory_status)" || {
        echo 'ERROR: effective build memory headroom cannot be measured' >&2; return 1;
    }
    read -r total_bytes available_bytes <<< "$measurement"
    free_percent=$((available_bytes * 100 / total_bytes))
    disk_mb="$(df -Pk "$root" | awk 'NR==2 {print int($4/1024)}')"
    [[ "$disk_mb" =~ ^[0-9]+$ ]] || { echo 'ERROR: build disk headroom cannot be measured' >&2; return 1; }
    if ((free_percent < minimum_percent || disk_mb < minimum_disk_mb)); then
        echo "ERROR: build headroom insufficient: effective_memory_free=${free_percent}% required=${minimum_percent}% disk_free=${disk_mb}MiB required=${minimum_disk_mb}MiB" >&2
        return 1
    fi
}

platform_monitor_build_headroom() {
    local build_pid="$1" root="$2" minimum_percent="$3" minimum_disk_mb="$4"
    while kill -0 "$build_pid" >/dev/null 2>&1; do
        if ! platform_assert_build_headroom "$root" "$minimum_percent" "$minimum_disk_mb"; then
            echo 'ERROR: cancelling this build to preserve service memory and disk headroom' >&2
            kill "$build_pid" >/dev/null 2>&1 || true
            wait "$build_pid" || true
            return 1
        fi
        sleep 10
    done
    wait "$build_pid"
}
