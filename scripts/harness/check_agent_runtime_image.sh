#!/usr/bin/env bash
# Check an already-local image only; never builds, pulls or touches app containers.
set -euo pipefail
runtime_image="${AI_PLATFORM_AGENT_RUNTIME_IMAGE:?Set the locally built Runtime image}"
artifact="$(docker image inspect "$runtime_image" --format '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.artifact"}}')"
binary="$(docker image inspect "$runtime_image" --format '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.binary"}}')"
if [[ "$artifact" != agent_runtime || "$binary" != ai-platform-agent-runtime ]]; then
    echo 'ERROR: expected the Agent Runtime artifact' >&2
    exit 2
fi
image_id="$(docker image inspect "$runtime_image" --format '{{.Id}}')"
docker run --rm --pull=never --network=none --cap-drop=ALL \
    --security-opt=no-new-privileges --pids-limit=64 --memory=256m --cpus=1 \
    --entrypoint /usr/local/libexec/ai-platform-agent-runtime-image-check "$image_id"
docker image inspect "$image_id" --format 'runtime-image: id={{.Id}} bytes={{.Size}} os={{.Os}} arch={{.Architecture}} user={{.Config.User}} workdir={{.Config.WorkingDir}}'
