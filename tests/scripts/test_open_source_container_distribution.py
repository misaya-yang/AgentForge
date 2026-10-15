import json
import os
import re
import shutil
import stat
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]


def _env_values(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text().splitlines():
        if not line or line.lstrip().startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key] = value
    return values


def _service_section(compose: str, service: str) -> str:
    match = re.search(
        rf"^  {re.escape(service)}:\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)",
        compose,
        flags=re.MULTILINE | re.DOTALL,
    )
    assert match, f"missing service: {service}"
    return match.group("body")


def _render_compose(**overrides: str) -> dict:
    if shutil.which("docker") is None:
        pytest.skip("docker CLI is required to verify rendered Compose config")

    # Prevent developer-shell credentials from entering the rendered test
    # config or a failure report. These deterministic values are not secrets.
    env = {
        **os.environ,
        "POSTGRES_PASSWORD": "a" * 64,
        "REDIS_PASSWORD": "b" * 64,
        "JWT_SECRET": "c" * 64,
        "AI_PLATFORM_INTERNAL_TOKEN": "d" * 64,
        "DEFAULT_USER_PASSWORD": "f" * 64,
        "DASHSCOPE_API_KEY": "test-general-key",
        "DASHSCOPE_CHAT_API_KEY": "test-chat-key",
        "DASHSCOPE_IMAGE_API_KEY": "test-image-key",
        "DASHSCOPE_EMBEDDING_API_KEY": "test-embedding-key",
        **overrides,
    }
    result = subprocess.run(
        [
            "docker",
            "compose",
            "--env-file",
            str(ROOT / ".env.example"),
            "config",
            "--format",
            "json",
        ],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def test_base_compose_uses_versioned_published_images_without_build_contexts() -> None:
    compose = (ROOT / "docker-compose.yml").read_text()

    assert "\n    build:\n" not in compose
    expected = {
        "gateway": "ghcr.io/misaya-yang/ai-gateway:2.0.0",
        "frontend": "ghcr.io/misaya-yang/ai-gateway-web:2.0.0",
        "knowledge-service": "ghcr.io/misaya-yang/ai-gateway-knowledge-service:2.0.0",
        "migrate": "ghcr.io/misaya-yang/ai-gateway-migrate:2.0.0",
    }
    for service, image in expected.items():
        section = _service_section(compose, service)
        assert image in section
        assert ":latest" not in section
    assert "\n  mcp-docgen-server:" not in compose


def test_source_build_overlay_owns_every_compose_build_context() -> None:
    overlay = (ROOT / "docker-compose.build.yml").read_text()
    expected = {
        "gateway": "Dockerfile",
        "frontend": "Dockerfile",
        "knowledge-service": "apps/knowledge-service/Dockerfile",
        "migrate": "docker/migrate/Dockerfile",
    }
    for service, dockerfile in expected.items():
        section = _service_section(overlay, service)
        assert "    build:\n" in section
        assert f"dockerfile: {dockerfile}" in section
        assert 'VCS_REF: "${VCS_REF:-unknown}"' in section
    assert "\n  mcp-docgen-server:" not in overlay


def test_service_database_search_paths_follow_schema_ownership() -> None:
    rendered = _render_compose(GATEWAY_DATABASE_AUTO_INIT="false")
    services = rendered["services"]

    gateway_env = services["gateway"]["environment"]
    knowledge_env = services["knowledge-service"]["environment"]

    assert gateway_env["GATEWAY_DATABASE__AUTO_INIT"] == "false"
    assert gateway_env["GATEWAY_DATABASE__DSN"].endswith(
        "?options=-csearch_path%3Dgateway%2Cassistant%2Cknowledge%2Cpublic"
    )
    assert knowledge_env["KNOWLEDGE_DATABASE__DSN"].endswith(
        "?options=-csearch_path%3Dknowledge%2Cgateway%2Cassistant%2Cpublic"
    )


def test_default_initializer_needs_only_dashscope_model_secret(tmp_path: Path) -> None:
    target = tmp_path / ".env"
    model_key = "test-dashscope-runtime-key"
    env = {
        **os.environ,
        "DASHSCOPE_API_KEY": model_key,
        "GATEWAY_DATABASE_AUTO_INIT": "false",
    }
    for key in (
        "POSTGRES_PASSWORD",
        "REDIS_PASSWORD",
        "JWT_SECRET",
        "AI_PLATFORM_INTERNAL_TOKEN",
        "GATEWAY_ENCRYPTION_KEY",
        "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
        "DEFAULT_USER_PASSWORD",
        "KB_EMBEDDING_API_KEY",
        "KB_EMBEDDING_PROVIDER",
    ):
        env.pop(key, None)

    result = subprocess.run(
        ["bash", "scripts/new/init-env.sh", "--env", str(target)],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert model_key not in output
    values = _env_values(target)
    assert values["DASHSCOPE_API_KEY"] == model_key
    assert values["KB_EMBEDDING_PROVIDER"] == "dashscope"
    assert values["KB_EMBEDDING_API_KEY"] == ""
    assert values["KB_EMBEDDING_MODEL"] == "text-embedding-v4"
    assert values["GATEWAY_DATABASE_AUTO_INIT"] == "false"
    for key in (
        "POSTGRES_PASSWORD",
        "REDIS_PASSWORD",
        "JWT_SECRET",
        "AI_PLATFORM_INTERNAL_TOKEN",
        "GATEWAY_ENCRYPTION_KEY",
        "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
        "DEFAULT_USER_PASSWORD",
    ):
        assert len(values[key]) >= 32
        assert "change_me" not in values[key]
    assert stat.S_IMODE(target.stat().st_mode) == 0o600


def test_initializer_backfills_runtime_trust_secrets_without_replacing_existing_env(
    tmp_path: Path,
) -> None:
    target = tmp_path / ".env"
    target.write_text("POSTGRES_PASSWORD=keep-this-value\n")

    result = subprocess.run(
        [
            "bash",
            "scripts/new/init-env.sh",
            "--env",
            str(target),
            "--if-missing",
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    values = _env_values(target)
    assert values["POSTGRES_PASSWORD"] == "keep-this-value"
    assert len(values["GATEWAY_ENCRYPTION_KEY"]) >= 32
    assert values["GATEWAY_ENCRYPTION_KEY"] not in output
    for key in (
        "AI_PLATFORM_INTERNAL_TOKEN",
        "AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET",
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
    ):
        assert len(values[key]) >= 32
        assert values[key] not in output
    assert stat.S_IMODE(target.stat().st_mode) == 0o600


def test_initializer_refreshes_only_stale_local_runtime_identity(tmp_path: Path) -> None:
    target = tmp_path / ".env"
    target.write_text(
        "AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION=" + "a" * 40 + "+" + "b" * 12 + "\n"
        "AI_PLATFORM_AGENT_RUNTIME_IMAGE=ai-gateway-agent-runtime:local-old-old\n"
    )

    result = subprocess.run(
        ["bash", "scripts/new/init-env.sh", "--env", str(target), "--if-missing"],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    values = _env_values(target)
    # Local source builds follow the current source receipt; the public
    # .env.example can intentionally retain the published release identity.
    receipt = json.loads((ROOT / "deploy/agent-runtime-source/source-receipt.json").read_text())
    upstream = receipt["source"]["upstream_sha"]
    overlay = receipt["overlay"]["sha256"]
    assert values["AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION"] == f"{upstream}+{overlay[:12]}"
    assert values["AI_PLATFORM_AGENT_RUNTIME_IMAGE"] == f"ai-gateway-agent-runtime:local-{upstream[:12]}-{overlay[:12]}"


def test_generated_admin_password_is_hashed_before_database_bootstrap() -> None:
    container = (ROOT / "src/container.py").read_text()
    database = (
        ROOT / "packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py"
    ).read_text()

    assert 'os.environ.get("DEFAULT_USER_PASSWORD", "")' in container
    assert "bootstrap_admin_password_hash = hash_password(" in container
    assert "bootstrap_admin_password_hash=bootstrap_admin_password_hash" in container
    assert "_ensure_bootstrap_admin_password_hash" in database
    assert "password_hash IS NULL OR password_hash = ''" in database
    assert "SET password_hash = $1" in database


def test_dockerfiles_do_not_accept_provider_secrets_as_build_arguments() -> None:
    dockerfiles = [
        ROOT / "Dockerfile",
        ROOT / "apps/knowledge-service/Dockerfile",
        ROOT / "web/Dockerfile",
        ROOT / "docker/migrate/Dockerfile",
        ROOT / "docker/code-interpreter/Dockerfile",
    ]
    for path in dockerfiles:
        text = path.read_text()
        assert not re.search(r"^ARG\s+.*(?:API_KEY|TOKEN|PASSWORD|SECRET)", text, re.MULTILINE)
        assert "COPY .env" not in text
        assert ":latest" not in text

    non_root = [
        ROOT / "Dockerfile",
        ROOT / "apps/knowledge-service/Dockerfile",
        ROOT / "docker/migrate/Dockerfile",
        ROOT / "docker/code-interpreter/Dockerfile",
    ]
    for path in non_root:
        assert re.search(r"^USER\s+\S+", path.read_text(), re.MULTILINE)

    assert not (ROOT / "apps" / ("assistant" + "-service")).exists()
    assert not (ROOT / "packages" / "mcp-docgen-server").exists()


def test_redis_runtime_config_keeps_secrets_out_of_compose_command(
    tmp_path: Path,
) -> None:
    compose = (ROOT / "docker-compose.yml").read_text()
    redis = _service_section(compose, "redis")
    common = (ROOT / "scripts/new/common.sh").read_text()
    entrypoint = (ROOT / "scripts/new/redis-entrypoint.sh").read_text()

    assert 'REDIS_MAXMEMORY: "${REDIS_MAXMEMORY:-128mb}"' in redis
    assert "command:\n      - /bin/sh\n      - /opt/ai-gateway/redis-entrypoint.sh" in redis
    command = redis.split("command:", 1)[1].split("ports:", 1)[0]
    assert "--requirepass" not in redis
    assert "REDIS_PASSWORD" not in command
    assert 'REDISCLI_AUTH=\\"$${REDIS_PASSWORD}\\" redis-cli ping' in redis
    assert "redis-cli -a" not in redis
    assert 'REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli ping' in common
    assert 'redis-cli -a "${REDIS_PASSWORD' not in common
    assert "exec /usr/local/bin/docker-entrypoint.sh redis-server" in entrypoint
    assert "chmod 600" in entrypoint
    assert 'if [ "${#redis_password}" -lt 8 ]' in entrypoint
    assert "od -An -v -tx1" in entrypoint
    assert 'printf "\\\\x%s", $i' in entrypoint
    assert 'printf \'requirepass "%s"\\n\' "$redis_password_escaped"' in entrypoint
    assert "printf 'requirepass %s" not in entrypoint
    assert "unset redis_password redis_password_escaped REDIS_PASSWORD" in entrypoint
    assert "^[1-9][0-9]*([kKmMgG][bB]?)?$" in entrypoint

    marker = tmp_path / "redis-maxmemory-injection"
    malicious = f"192mb;touch {marker}"
    for invalid_maxmemory in (malicious, '192mb\nsave ""', "192mib", "0"):
        result = subprocess.run(
            ["sh", str(ROOT / "scripts/new/redis-entrypoint.sh")],
            env={
                **os.environ,
                "REDIS_PASSWORD": "a" * 64,
                "REDIS_MAXMEMORY": invalid_maxmemory,
            },
            text=True,
            capture_output=True,
            check=False,
        )
        assert result.returncode == 64
    assert not marker.exists()

    rendered = _render_compose(REDIS_MAXMEMORY=malicious)
    rendered_redis = rendered["services"]["redis"]
    assert rendered_redis["environment"]["REDIS_MAXMEMORY"] == malicious
    assert malicious not in " ".join(rendered_redis["command"])
    assert "test-general-key" not in " ".join(rendered_redis["command"])


def test_dashscope_endpoint_overrides_are_documented_and_injected() -> None:
    credential_vars = (
        "DASHSCOPE_API_KEY",
        "DASHSCOPE_CHAT_API_KEY",
        "DASHSCOPE_IMAGE_API_KEY",
        "DASHSCOPE_EMBEDDING_API_KEY",
    )
    ocr_vars = ("DASHSCOPE_OCR_API_KEY", "DASHSCOPE_OCR_BASE_URL")
    endpoint_vars = (
        "DASHSCOPE_BASE_URL",
        "DASHSCOPE_CHAT_BASE_URL",
        "DASHSCOPE_EMBEDDING_BASE_URL",
        "DASHSCOPE_IMAGE_BASE_URL",
    )
    env_values = _env_values(ROOT / ".env.example")
    compose = (ROOT / "docker-compose.yml").read_text()

    for key in (*credential_vars, *endpoint_vars):
        assert env_values[key] == ""

    for service in ("gateway", "knowledge-service"):
        section = _service_section(compose, service)
        for key in (*credential_vars, *endpoint_vars):
            assert f'{key}: "${{{key}:-}}"' in section
    knowledge_section = _service_section(compose, "knowledge-service")
    for key in ocr_vars:
        assert env_values[key] == ""
        assert f'{key}: "${{{key}:-}}"' in knowledge_section

    rerank_vars = (
        "DASHSCOPE_RERANK_BASE_URL",
        "DASHSCOPE_RERANK_INSTRUCT",
    )
    for key in rerank_vars:
        assert env_values[key] == ""
        assert f'{key}: "${{{key}:-}}"' in _service_section(compose, "knowledge-service")
    assert env_values["DASHSCOPE_RERANK_REQUEST_SCHEMA"] == "auto"
    assert (
        'DASHSCOPE_RERANK_REQUEST_SCHEMA: "${DASHSCOPE_RERANK_REQUEST_SCHEMA:-auto}"'
        in _service_section(compose, "knowledge-service")
    )

    initializer = (ROOT / "scripts/new/init-env.sh").read_text()
    copied_keys_match = re.search(
        r'^COPIED_KEYS=""\nfor key in (?P<keys>.*?); do$',
        initializer,
        flags=re.MULTILINE,
    )
    assert copied_keys_match
    copied_keys = set(copied_keys_match.group("keys").split())
    for key in (
        *credential_vars,
        *endpoint_vars,
        *rerank_vars,
        "DASHSCOPE_RERANK_REQUEST_SCHEMA",
        *ocr_vars,
    ):
        assert key in initializer
    for key in (*rerank_vars, "DASHSCOPE_RERANK_REQUEST_SCHEMA", *ocr_vars):
        assert key in copied_keys

    rendered = _render_compose(
        DASHSCOPE_OCR_API_KEY="test-ocr-key",
        DASHSCOPE_OCR_BASE_URL="https://ocr.example.test",
    )
    expected_credentials = {
        "DASHSCOPE_API_KEY": "test-general-key",
        "DASHSCOPE_CHAT_API_KEY": "test-chat-key",
        "DASHSCOPE_IMAGE_API_KEY": "test-image-key",
        "DASHSCOPE_EMBEDDING_API_KEY": "test-embedding-key",
    }
    for service in ("gateway", "knowledge-service"):
        environment = rendered["services"][service]["environment"]
        for key, expected in expected_credentials.items():
            assert environment[key] == expected
    assert (
        rendered["services"]["knowledge-service"]["environment"][
            "KNOWLEDGE_EMBEDDINGS__DASHSCOPE_API_KEY"
        ]
        == "test-embedding-key"
    )
    assert (
        rendered["services"]["knowledge-service"]["environment"][
            "DASHSCOPE_OCR_API_KEY"
        ]
        == "test-ocr-key"
    )
    assert (
        rendered["services"]["knowledge-service"]["environment"][
            "DASHSCOPE_OCR_BASE_URL"
        ]
        == "https://ocr.example.test"
    )


def test_qwen_responses_wire_default_is_owned_by_the_gateway_model_plane() -> None:
    env_values = _env_values(ROOT / ".env.example")
    compose = (ROOT / "docker-compose.yml").read_text()

    assert env_values["DASHSCOPE_CHAT_WIRE_PROTOCOL"] == "responses_v1"
    assert env_values["OPENAI_BASE_URL"] == ""
    assert env_values["OPENAI_WIRE_PROTOCOL"] == "chat_completions"

    section = _service_section(compose, "gateway")
    assert (
        'DASHSCOPE_CHAT_WIRE_PROTOCOL: "${DASHSCOPE_CHAT_WIRE_PROTOCOL:-responses_v1}"'
    ) in section
    assert 'OPENAI_BASE_URL: "${OPENAI_BASE_URL:-}"' in section
    assert 'OPENAI_WIRE_PROTOCOL: "${OPENAI_WIRE_PROTOCOL:-chat_completions}"' in section

    initializer = (ROOT / "scripts/new/init-env.sh").read_text()
    copied_keys_match = re.search(
        r'^COPIED_KEYS=""\nfor key in (?P<keys>.*?); do$',
        initializer,
        flags=re.MULTILINE,
    )
    assert copied_keys_match
    copied_keys = set(copied_keys_match.group("keys").split())
    for key in (
        "DASHSCOPE_CHAT_WIRE_PROTOCOL",
        "OPENAI_BASE_URL",
        "OPENAI_WIRE_PROTOCOL",
    ):
        assert key in copied_keys


def test_publish_workflow_covers_all_images_and_both_cpu_architectures() -> None:
    workflow = (ROOT / ".github/workflows/docker-publish.yml").read_text()
    for image in (
        "ai-gateway",
        "ai-gateway-web",
        "ai-gateway-knowledge-service",
        "ai-gateway-migrate",
        "ai-gateway-code-interpreter",
    ):
        assert f"image: {image}" in workflow
    assert "platforms: linux/amd64,linux/arm64" in workflow
    assert "docker/setup-qemu-action@v3" in workflow
    assert "provenance: mode=max" in workflow
    assert "sbom: true" in workflow
    assert "type=semver,pattern={{version}}" in workflow
    assert "type=sha,format=long" in workflow
    assert "type=semver,pattern={{major}}.{{minor}}" not in workflow
    assert "type=raw,value=latest" not in workflow
    assert "islamic-content-service" not in workflow


def test_code_interpreter_image_is_non_interactive_by_default() -> None:
    interpreter = (ROOT / "docker/code-interpreter/Dockerfile").read_text()
    assert 'CMD ["python"]' in interpreter
    assert "/workspace/main.py" not in interpreter


def test_code_execution_is_owned_by_the_worker_without_a_docker_socket_overlay() -> None:
    base_compose = (ROOT / "docker-compose.yml").read_text()
    makefile = (ROOT / "Makefile").read_text()
    catalog = (
        ROOT
        / "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json"
    ).read_text()

    assert '"name": "execute_python_code"' in catalog
    assert "/var/run/docker.sock" not in base_compose
    assert not (ROOT / "docker-compose.code-executor.yml").exists()
    assert "code-executor-enable:" not in makefile


def test_default_quickstart_never_builds_source() -> None:
    makefile = (ROOT / "Makefile").read_text()
    quickstart = re.search(
        r"^quickstart:.*?(?=^[a-zA-Z_-]+:)",
        makefile,
        flags=re.MULTILINE | re.DOTALL,
    )
    source_build = re.search(
        r"^quickstart-build:.*?(?=^[a-zA-Z_-]+:)",
        makefile,
        flags=re.MULTILINE | re.DOTALL,
    )
    assert quickstart and source_build
    assert "--pull" in quickstart.group(0)
    assert "--build" not in quickstart.group(0)
    assert "--build" in source_build.group(0)
    assert "COMPOSE_PARALLEL_LIMIT ?= 1" in makefile


def test_default_complete_stack_memory_ceiling_stays_below_3_5_gib() -> None:
    compose = (ROOT / "docker-compose.yml").read_text()
    limits = [int(value) for value in re.findall(r'mem_limit: "\$\{[A-Z0-9_]+:-(\d+)m\}"', compose)]

    assert len(limits) == 8
    assert sum(limits) + 1_024 <= 3_584
    assert 'mem_limit: "${AGENT_CAPABILITY_WORKER_MEMORY_LIMIT:-1g}"' in compose
    assert 'mem_limit: "${KNOWLEDGE_WORKER_MEMORY_LIMIT:-512m}"' in compose
    assert 'REDIS_MAXMEMORY: "${REDIS_MAXMEMORY:-128mb}"' in compose
    assert 'GATEWAY_TASK_WORKER_CONCURRENCY: "${GATEWAY_TASK_WORKER_CONCURRENCY:-1}"' in compose
    assert (
        'KNOWLEDGE_PROCESSING__WORKER_CONCURRENCY: "${GATEWAY_KNOWLEDGE__WORKER_CONCURRENCY:-1}"'
    ) in compose


def test_standalone_dev_runtime_uses_pinned_images_and_memory_guards() -> None:
    script = (ROOT / "scripts/new/setup-dev.sh").read_text()

    assert 'DEV_PG_CONTAINER="${POSTGRES_CONTAINER:-ai-gateway-pg}"' in script
    assert 'DEV_QDRANT_IMAGE="${QDRANT_IMAGE:-qdrant/qdrant:v1.18.2}"' in script
    assert "qdrant/qdrant:latest" not in script
    assert "assert_compose_owner" in script
    assert '--memory "${POSTGRES_MEMORY_LIMIT:-320m}"' in script
    assert '--memory "${REDIS_MEMORY_LIMIT:-192m}"' in script
    assert '--memory "${QDRANT_MEMORY_LIMIT:-352m}"' in script
    assert "this checkout requires $image" in script
    assert '-p "127.0.0.1:${PG_PORT}:5432"' in script
    assert '-p "127.0.0.1:${REDIS_DEV_PORT}:6379"' in script
    assert "redis-cli -a $REDIS_PASS" not in script
