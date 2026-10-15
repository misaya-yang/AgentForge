#!/bin/sh
set -eu

# Keep secrets out of Docker Config.Cmd and the redis-server process argv.
# New installs use a 64-character hexadecimal password, but existing env files
# may contain older non-hex values accepted by validate-env.sh. Preserve that
# compatibility by encoding every password byte as a Redis quoted-string hex
# escape before it enters the generated config; raw password bytes never become
# config syntax or process arguments.
redis_password="${REDIS_PASSWORD:-}"
if [ "${#redis_password}" -lt 8 ]; then
    echo "REDIS_PASSWORD must contain at least 8 characters" >&2
    exit 64
fi
redis_password_escaped="$(
    printf '%s' "$redis_password" \
        | od -An -v -tx1 \
        | awk '{ for (i = 1; i <= NF; i++) printf "\\x%s", $i }'
)"
if [ -z "$redis_password_escaped" ]; then
    echo "REDIS_PASSWORD could not be encoded for the runtime config" >&2
    exit 70
fi

# Redis accepts byte counts plus k/kb, m/mb, or g/gb suffixes. Reject shell
# metacharacters and ambiguous values before writing the runtime config.
redis_maxmemory="${REDIS_MAXMEMORY:-}"
case "$redis_maxmemory" in
    ''|*[!0123456789kKmMgGbB]*)
        echo "REDIS_MAXMEMORY must be a positive integer with an optional k, kb, m, mb, g, or gb suffix" >&2
        exit 64
        ;;
esac
if ! printf '%s\n' "$redis_maxmemory" | grep -Eq '^[1-9][0-9]*([kKmMgG][bB]?)?$'; then
    echo "REDIS_MAXMEMORY must be a positive integer with an optional k, kb, m, mb, g, or gb suffix" >&2
    exit 64
fi

runtime_config="/tmp/ai-gateway-redis.conf"
umask 077
{
    printf '%s\n' 'appendonly yes'
    printf 'requirepass "%s"\n' "$redis_password_escaped"
    printf 'maxmemory %s\n' "$redis_maxmemory"
    printf '%s\n' 'maxmemory-policy allkeys-lru'
} > "$runtime_config"
chown redis:redis "$runtime_config"
chmod 600 "$runtime_config"

# Re-enter the image's official entrypoint so it prepares /data and drops from
# root to the redis user before replacing PID 1 with redis-server. Redis no
# longer needs the plaintext environment value after the config is sealed.
unset redis_password redis_password_escaped REDIS_PASSWORD
exec /usr/local/bin/docker-entrypoint.sh redis-server "$runtime_config"
