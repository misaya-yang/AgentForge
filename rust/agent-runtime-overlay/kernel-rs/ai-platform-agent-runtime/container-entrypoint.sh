#!/bin/sh
set -eu

runtime_home="${AI_PLATFORM_AGENT_HOME:?AI_PLATFORM_AGENT_HOME is required}"
case "$runtime_home" in
    /*) ;;
    *)
        echo "ERROR: AI_PLATFORM_AGENT_HOME must be absolute" >&2
        exit 2
        ;;
esac

if [ -L "$runtime_home" ]; then
    echo "ERROR: AI_PLATFORM_AGENT_HOME must not be a symlink" >&2
    exit 2
fi
mkdir -p "$runtime_home"

marker="$runtime_home/.ai-platform-runtime-home"
if [ ! -e "$marker" ]; then
    if [ -n "$(find "$runtime_home" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        echo "ERROR: AI_PLATFORM_AGENT_HOME must be empty on first initialization" >&2
        exit 2
    fi
    printf '%s\n' 'ai-platform-agent-home/v1' > "$marker"
fi

exec /usr/local/bin/ai-platform-agent-runtime "$@"
