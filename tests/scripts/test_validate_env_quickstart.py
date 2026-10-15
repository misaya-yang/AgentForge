from __future__ import annotations

import json
import os
import re
import secrets
import stat
import subprocess
from pathlib import Path


def _valid_env_text(*, secret: str, chat_assignment: str) -> str:
    env_text = Path(".env.example").read_text()
    env_text = env_text.replace("change_me_generate_with_openssl_32_bytes_minimum", secret)
    env_text = env_text.replace("change_me_generate_with_openssl", secret)
    env_text = env_text.replace("change_me_embedding_provider_key", "test-embedding-key")
    env_text = _set_env_value(env_text, "MODEL_SETUP_MODE", "environment")
    return f"{env_text}\n{chat_assignment}\n"


def _infra_only_env_text(*, secret: str) -> str:
    env_text = Path(".env.example").read_text()
    env_text = re.sub(
        r"^POSTGRES_PASSWORD=.*$",
        f"POSTGRES_PASSWORD={secret}",
        env_text,
        flags=re.MULTILINE,
    )
    env_text = re.sub(
        r"^REDIS_PASSWORD=.*$",
        f"REDIS_PASSWORD={secret}",
        env_text,
        flags=re.MULTILINE,
    )
    return env_text


def _set_env_value(env_text: str, key: str, value: str) -> str:
    return re.sub(rf"^{key}=.*$", f"{key}={value}", env_text, flags=re.MULTILINE)


def _write_fake_runtime_commands(tmp_path: Path) -> Path:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_docker = fake_bin / "docker"
    fake_docker.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = "compose" ]; then\n'
        '  case "$*" in *" config"*) exit 0 ;; esac\n'
        '  case "$2" in\n'
        "    version) exit 0 ;;\n"
        "    --env-file) exit 0 ;;\n"
        "  esac\n"
        "fi\n"
        'if [ "$1" = "info" ]; then exit 0; fi\n'
        'if [ "$1" = "ps" ]; then echo ai-gateway-pg; exit 0; fi\n'
        'if [ "$1" = "exec" ]; then\n'
        '  case "$*" in\n'
        "    *redis-cli*) echo PONG; exit 0 ;;\n"
        "    *psql*)\n"
        '      case "$*" in\n'
        "        *qwen3.7-plus*) printf '%s\\n' \"${FAKE_DEFAULT_QWEN37_COUNT:-1}\"; exit 0 ;;\n"
        "        *provider_id*) printf '%s\\n' \"$FAKE_ENABLED_MODEL_PROVIDERS\"; exit 0 ;;\n"
        "        *) echo 1; exit 0 ;;\n"
        "      esac\n"
        "      ;;\n"
        "    *curl*) exit 0 ;;\n"
        "  esac\n"
        "fi\n"
        "exit 1\n"
    )
    fake_docker.chmod(fake_docker.stat().st_mode | stat.S_IXUSR)

    fake_curl = fake_bin / "curl"
    fake_curl.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        '  *"/metrics"*)\n'
        "    echo '# HELP gateway_up Gateway metrics endpoint availability'\n"
        "    echo '# TYPE gateway_up gauge'\n"
        "    echo 'gateway_up 1'\n"
        "    exit 0\n"
        "    ;;\n"
        "esac\n"
        "exit 0\n"
    )
    fake_curl.chmod(fake_curl.stat().st_mode | stat.S_IXUSR)
    return fake_bin


def _compose_service_section(compose: str, service: str) -> str:
    match = re.search(
        rf"^  {re.escape(service)}:\n(?P<body>.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)",
        compose,
        flags=re.MULTILINE | re.DOTALL,
    )
    assert match, f"{service} service missing from docker-compose.yml"
    return match.group("body")


def _run_validate_env(
    tmp_path: Path,
    env_text: str,
    *,
    args: list[str],
    enabled_model_providers: str = "",
    default_qwen37_count: str = "1",
) -> subprocess.CompletedProcess[str]:
    env_file = tmp_path / ".env"
    env_file.write_text(env_text)
    fake_bin = _write_fake_runtime_commands(tmp_path)
    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
        "FAKE_ENABLED_MODEL_PROVIDERS": enabled_model_providers,
        "FAKE_DEFAULT_QWEN37_COUNT": default_qwen37_count,
    }
    return subprocess.run(
        ["bash", "scripts/new/validate-env.sh", "--env", str(env_file), *args],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )


def test_validate_env_accepts_documented_local_bootstrap_password(tmp_path: Path) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(
        env_text,
        "DEFAULT_USER_PASSWORD",
        "ChangeMe-Admin-2026!",
    )
    result = _run_validate_env(
        tmp_path,
        env_text,
        args=["--config-only"],
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Configuration validation passed" in output
    assert "DEFAULT_USER_PASSWORD uses the documented local bootstrap default" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_example_mode_accepts_committed_example(tmp_path: Path) -> None:
    example_text = Path(".env.example").read_text()
    result = _run_validate_env(tmp_path, example_text, args=["--example"])

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Example configuration validation passed" in output
    assert "change_me_generate_with_openssl" not in output
    assert "change_me_embedding_provider_key" not in output


def test_validate_env_example_mode_ignores_runtime_flag_order(tmp_path: Path) -> None:
    example_text = Path(".env.example").read_text()
    result = _run_validate_env(tmp_path, example_text, args=["--example", "--runtime"])

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Example configuration validation passed" in output
    assert "Validating runtime dependencies" not in output


def test_validate_env_config_still_rejects_committed_example_for_release(
    tmp_path: Path,
) -> None:
    example_text = Path(".env.example").read_text()
    result = _run_validate_env(tmp_path, example_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "POSTGRES_PASSWORD must be set to a non-placeholder secret" in output
    assert "REDIS_PASSWORD must be set to a non-placeholder secret" in output
    assert "Example configuration validation passed" not in output
    assert "change_me_generate_with_openssl" not in output


def test_validate_env_default_model_optional_round_trip(tmp_path: Path) -> None:
    secret = secrets.token_hex(32)
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment="DASHSCOPE_API_KEY=test-chat-key",
    )

    # Unset: valid — DEFAULT_MODEL is optional. Each run gets a fresh
    # directory: _run_validate_env seeds tmp_path/bin once.
    for run_dir in ("unset", "set", "empty"):
        (tmp_path / run_dir).mkdir()
    result = _run_validate_env(tmp_path / "unset", env_text, args=["--config-only"])
    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Configuration validation passed" in output

    # Set to a non-empty value: valid.
    result = _run_validate_env(
        tmp_path / "set",
        _set_env_value(env_text, "DEFAULT_MODEL", "my-deployment-model"),
        args=["--config-only"],
    )
    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Configuration validation passed" in output

    # Set but empty: fails the optional non-empty format check.
    result = _run_validate_env(
        tmp_path / "empty",
        _set_env_value(env_text, "DEFAULT_MODEL", ""),
        args=["--config-only"],
    )
    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "DEFAULT_MODEL must be non-empty when set" in output


def test_compose_injects_the_default_model_into_the_gateway_model_plane() -> None:
    compose = Path("docker-compose.yml").read_text()
    expected = 'DEFAULT_MODEL: "${DEFAULT_MODEL:-qwen3.8-flash}"'

    section = _compose_service_section(compose, "gateway")
    assert expected in section


def test_validate_env_rejects_non_release_application_image_tag(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(
        env_text,
        "GATEWAY_IMAGE",
        "ghcr.io/misaya-yang/ai-gateway:nightly",
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "GATEWAY_IMAGE must use a full semantic release tag (for example 2.0.0) or sha256 digest."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_example_mode_requires_public_env_shape(tmp_path: Path) -> None:
    example_text = re.sub(
        r"^AI_PLATFORM_INTERNAL_TOKEN=.*\n",
        "",
        Path(".env.example").read_text(),
        flags=re.MULTILINE,
    )
    result = _run_validate_env(tmp_path, example_text, args=["--example"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "AI_PLATFORM_INTERNAL_TOKEN must be declared in the example env file" in output
    assert "change_me_generate_with_openssl" not in output


def test_validate_env_allows_first_boot_into_ui_model_setup(tmp_path: Path) -> None:
    secret = secrets.token_hex(32)
    env_text = _valid_env_text(secret=secret, chat_assignment="")
    env_text = _set_env_value(env_text, "MODEL_SETUP_MODE", "ui")

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "starting in UI model-setup mode" in output
    assert secret not in output


def test_validate_env_runtime_rejects_enabled_models_without_configured_provider(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-dashscope-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(secret=secret, chat_assignment=f"DASHSCOPE_API_KEY={chat_key}"),
        args=["--runtime"],
        enabled_model_providers="google|5",
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "No enabled chat models match configured providers" in output
    assert "Configured providers: dashscope" in output
    assert "Enabled model providers: google:5" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_runtime_accepts_enabled_model_for_configured_provider(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-dashscope-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(secret=secret, chat_assignment=f"DASHSCOPE_API_KEY={chat_key}"),
        args=["--runtime"],
        enabled_model_providers="dashscope|1",
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Chat model/provider alignment is valid (1 available model(s))." in output
    assert "Runtime validation passed" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_runtime_accepts_vertex_chat_provider(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-vertex-chat-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(
            secret=secret,
            chat_assignment=(
                "DASHSCOPE_API_KEY=test-dashscope-key\n"
                "GOOGLE_CHAT_BACKEND=vertex\n"
                f"VERTEX_CHAT_API_KEY={chat_key}"
            ),
        ),
        args=["--runtime"],
        enabled_model_providers="dashscope|1\ngoogle-vertex|1",
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Chat model/provider alignment is valid (2 available model(s))." in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_runtime_accepts_google_models_on_vertex_chat_backend(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-vertex-chat-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(
            secret=secret,
            chat_assignment=(
                "DASHSCOPE_API_KEY=test-dashscope-key\n"
                "GOOGLE_CHAT_BACKEND=vertex\n"
                f"VERTEX_CHAT_API_KEY={chat_key}"
            ),
        ),
        args=["--runtime"],
        enabled_model_providers="dashscope|1\ngoogle|1",
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Chat model/provider alignment is valid (2 available model(s))." in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_runtime_rejects_vertex_chat_key_without_vertex_backend(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-vertex-chat-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(secret=secret, chat_assignment=f"VERTEX_CHAT_API_KEY={chat_key}"),
        args=["--runtime"],
        enabled_model_providers="google-vertex|1",
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Set at least one supported chat provider key" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_config_rejects_vertex_chat_key_without_vertex_backend(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-vertex-chat-key"
    result = _run_validate_env(
        tmp_path,
        _valid_env_text(secret=secret, chat_assignment=f"VERTEX_CHAT_API_KEY={chat_key}"),
        args=["--config-only"],
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Set at least one supported chat provider key" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_skips_compose_when_required_config_is_invalid(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}\nDEFAULT_USER_PASSWORD=",
    )
    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "DEFAULT_USER_PASSWORD must be set to a non-placeholder secret" in output
    assert "Skipping docker compose config validation" in output
    assert "docker compose config validation failed" not in output
    assert "DEFAULT_USER_PASSWORD is required" not in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_infra_only_does_not_require_application_secrets(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    result = _run_validate_env(
        tmp_path,
        _infra_only_env_text(secret=secret),
        args=["--infra-only", "--config-only"],
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Infrastructure configuration validation passed" in output
    assert "JWT_SECRET" not in output
    assert "AI_PLATFORM_INTERNAL_TOKEN" not in output
    assert secret not in output

    script = Path("scripts/new/validate-env.sh").read_text()
    assert "validate_compose_config --no-interpolate postgres redis qdrant" in script


def test_compose_keeps_internal_service_ports_private() -> None:
    compose = Path("docker-compose.yml").read_text()

    private_bindings = {
        "postgres": ['"127.0.0.1:${POSTGRES_PORT:-5432}:5432"'],
        "redis": ['"127.0.0.1:${REDIS_PORT:-6379}:6379"'],
        "qdrant": [
            '"127.0.0.1:${QDRANT_HTTP_PORT:-6333}:6333"',
            '"127.0.0.1:${QDRANT_GRPC_PORT:-6334}:6334"',
        ],
        "tempo": ['"127.0.0.1:3200:3200"', '"127.0.0.1:4317:4317"'],
        "knowledge-service": ['"127.0.0.1:${KNOWLEDGE_SERVICE_PORT:-8092}:8092"'],
    }

    for service, bindings in private_bindings.items():
        section = _compose_service_section(compose, service)
        for binding in bindings:
            assert binding in section, f"{service} does not bind {binding}"

    runtime = _compose_service_section(compose, "agent-runtime")
    worker = _compose_service_section(compose, "agent-capability-worker")
    assert "\n    ports:" not in runtime
    assert '\n    expose:\n      - "8094"' in runtime
    assert "\n    ports:" not in worker
    assert '\n    expose:\n      - "8095"' in worker
    assert "8765" not in compose


def test_compose_forwards_the_single_model_setup_mode_contract() -> None:
    example = Path(".env.example").read_text()
    gateway = _compose_service_section(Path("docker-compose.yml").read_text(), "gateway")

    assert "MODEL_SETUP_MODE=ui" in example
    assert "GATEWAY_MODEL_SETUP_MODE" not in example
    assert 'MODEL_SETUP_MODE: "${MODEL_SETUP_MODE:-ui}"' in gateway


def test_validate_env_rejects_localhost_cors_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON must not include localhost origins "
        "when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_wildcard_cors_origin(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["*"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON must not use wildcard origins." in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_non_http_cors_origin(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["chrome-extension://ai-gateway"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON must be a JSON array of explicit http(s) origins."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_http_cors_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["http://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON must use https origins "
        "when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_accepts_explicit_https_cors_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_AUTH_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Configuration validation passed" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_frontend_auth_domain_mismatch_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_AUTH_EMAIL_DOMAIN", "example.com")
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "VITE_AUTH_EMAIL_DOMAIN must match AUTH_ALLOWED_EMAIL_DOMAIN when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_example_support_email_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_AUTH_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_SUPPORT_EMAIL", "admin@example.com")
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "VITE_SUPPORT_EMAIL must not use example.com when AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_http_frontend_runtime_url_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_API_URL", "http://api.myapp.test")
    env_text = _set_env_value(env_text, "VITE_API_BASE_URL", "http://api.myapp.test")
    env_text = _set_env_value(
        env_text,
        "VITE_TELEMETRY_ENDPOINT",
        "http://telemetry.myapp.test/events",
    )
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "VITE_API_URL must use https:// or a same-origin path when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert (
        "VITE_API_BASE_URL must use https:// or a same-origin path when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert (
        "VITE_TELEMETRY_ENDPOINT must use https:// or a same-origin path when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_rejects_local_frontend_runtime_url_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_API_URL", "http://localhost:8080")
    env_text = _set_env_value(env_text, "VITE_API_BASE_URL", "https://[::1]:8080")
    env_text = _set_env_value(
        env_text,
        "VITE_TELEMETRY_ENDPOINT",
        "https://127.0.0.1:8080/telemetry",
    )
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert (
        "VITE_API_URL must use https:// or a same-origin path when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert (
        "VITE_API_URL must not use localhost or loopback when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert (
        "VITE_API_BASE_URL must not use localhost or loopback when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert (
        "VITE_TELEMETRY_ENDPOINT must not use localhost or loopback when "
        "AUTH_ALLOWED_EMAIL_DOMAIN is non-local."
    ) in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_accepts_same_origin_frontend_runtime_paths_for_non_local_auth_domain(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    chat_key = "test-chat-key"
    env_text = _valid_env_text(
        secret=secret,
        chat_assignment=f"DASHSCOPE_API_KEY={chat_key}",
    )
    env_text = _set_env_value(env_text, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_AUTH_EMAIL_DOMAIN", "myapp.test")
    env_text = _set_env_value(env_text, "VITE_API_URL", "/api")
    env_text = _set_env_value(env_text, "VITE_API_BASE_URL", "/api/v1")
    env_text = _set_env_value(env_text, "VITE_TELEMETRY_ENDPOINT", "/telemetry")
    env_text = _set_env_value(
        env_text,
        "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )
    env_text = _set_env_value(
        env_text,
        "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
        '["https://ai.myapp.test"]',
    )

    result = _run_validate_env(tmp_path, env_text, args=["--config-only"])

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Configuration validation passed" in output
    assert secret not in output
    assert chat_key not in output


def test_validate_env_infra_only_ignores_missing_app_only_compose_vars(
    tmp_path: Path,
) -> None:
    secret = secrets.token_hex(32)
    env_text = _infra_only_env_text(secret=secret)
    for key in [
        "DEFAULT_USER_PASSWORD",
        "JWT_SECRET",
        "AI_PLATFORM_INTERNAL_TOKEN",
        "AUTH_ALLOWED_EMAIL_DOMAIN",
    ]:
        env_text = re.sub(rf"^{key}=.*\n?", "", env_text, flags=re.MULTILINE)

    result = _run_validate_env(
        tmp_path,
        env_text,
        args=["--infra-only", "--config-only"],
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Infrastructure configuration validation passed" in output
    assert "DEFAULT_USER_PASSWORD is required" not in output
    assert secret not in output


def test_vertex_chat_env_vars_are_documented_and_injected() -> None:
    expected_vars = ["GOOGLE_CHAT_BACKEND", "VERTEX_CHAT_API_KEY"]
    files = [
        Path(".env.example"),
        Path("README.md"),
        Path("docker-compose.yml"),
        Path("scripts/new/validate-env.sh"),
    ]

    for path in files:
        text = path.read_text()
        for key in expected_vars:
            assert key in text, f"{key} missing from {path}"


def test_frontend_support_email_defaults_follow_auth_domain() -> None:
    compose = Path("docker-compose.yml").read_text()
    dockerfile = Path("web/Dockerfile").read_text()

    assert (
        'VITE_SUPPORT_EMAIL: "${VITE_SUPPORT_EMAIL:-admin@${AUTH_ALLOWED_EMAIL_DOMAIN:-example.com}}"'
        in compose
    )
    assert "ARG VITE_SUPPORT_EMAIL=\n" in dockerfile
    assert "ARG VITE_SUPPORT_EMAIL=admin@example.com" not in dockerfile


def test_frontend_builder_matches_ci_toolchain() -> None:
    package = json.loads(Path("web/package.json").read_text())
    dockerfile = Path("web/Dockerfile").read_text()

    assert package["engines"]["node"] == "^26.11.1"
    assert package["packageManager"] == "pnpm@12.11.2"
    assert "ARG NODE_BASE_IMAGE=node:26.11.1-alpine3.24" in dockerfile
    assert "FROM ${NODE_BASE_IMAGE} AS builder" in dockerfile
    assert "npm install --global pnpm@12.11.2 --allow-scripts=pnpm" in dockerfile
    assert "COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./" in dockerfile
    assert "corepack enable" not in dockerfile
    assert "corepack prepare" not in dockerfile


def test_deploy_app_includes_application_microservices() -> None:
    script = Path("scripts/new/deploy.sh").read_text()
    common = Path("scripts/new/common.sh").read_text()

    assert 'FULL_APP_SERVICES="$(topology_service_ids app)"' in script
    assert 'INFRA_SERVICES="$(topology_service_ids infrastructure)"' in script
    assert '--service-ids "$scope"' in common
    assert "if topology_service_present knowledge-worker; then" in script
    assert 'wait_for_healthy "Capability worker"' in script


def test_deploy_pull_uses_selected_service_scope() -> None:
    script = Path("scripts/new/deploy.sh").read_text()

    assert script.index("# -- Determine services to deploy") < script.index("# -- Pull base images")
    assert re.search(r"pull \$SERVICES\b", script)


def test_deploy_rejects_infra_and_app_together() -> None:
    result = subprocess.run(
        ["bash", "scripts/new/deploy.sh", "--infra", "--app"],
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 2, output
    assert "--infra and --app cannot be used together" in output
    assert "Pre-flight checks" not in output


def test_deploy_app_migration_waits_for_postgres() -> None:
    script = Path("scripts/new/deploy.sh").read_text()

    assert "SHOULD_MIGRATE=false" in script
    assert (
        'if [ "$SKIP_MIGRATE" != true ] && [ "$INFRA_ONLY" != true ]; then\n'
        "    SHOULD_MIGRATE=true\n"
        "fi"
    ) in script
    assert (
        'if [ "$APP_ONLY" != true ] || [ "$SHOULD_MIGRATE" = true ]; then\n'
        '    wait_for_healthy "PostgreSQL" "check_postgres_health" 30\n'
        "fi"
    ) in script
    assert 'if [ "$SHOULD_MIGRATE" = true ]; then' in script


def test_validate_runtime_checks_gateway_metrics_endpoint() -> None:
    common = Path("scripts/new/common.sh").read_text()
    validate = Path("scripts/new/validate-env.sh").read_text()
    status = Path("scripts/new/status.sh").read_text()

    assert "check_gateway_metrics()" in common
    assert "http://localhost:${GATEWAY_PORT:-8080}/metrics" in common
    assert "grep -Eq '^# HELP |^# TYPE '" in common
    assert "grep -Eq '^gateway_up($|[ {])'" in common
    assert '[ -z "$body" ] && return 0' not in common
    assert 'wait_for_healthy "Gateway metrics endpoint" "check_gateway_metrics" 60' in validate
    assert 'fail "Gateway metrics check failed."' in validate
    assert 'check_and_report "Gateway metrics" check_gateway_metrics' in status


def test_gateway_metrics_helper_requires_gateway_up_metric(tmp_path: Path) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_curl = fake_bin / "curl"
    fake_curl.write_text(
        "#!/bin/sh\n"
        'case "$METRICS_CASE" in\n'
        "  empty) exit 0 ;;\n"
        "  generic) echo '# HELP other_metric Other'; echo 'other_metric 1'; exit 0 ;;\n"
        "  gateway) echo '# HELP gateway_up Gateway metrics endpoint availability'; echo 'gateway_up 1'; exit 0 ;;\n"
        "  protected)\n"
        '    case "$*" in\n'
        "      *\"%{http_code}\"*) printf '403'; exit 0 ;;\n"
        "      *) exit 22 ;;\n"
        "    esac\n"
        "    ;;\n"
        "esac\n"
        "exit 1\n"
    )
    fake_curl.chmod(fake_curl.stat().st_mode | stat.S_IXUSR)

    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    command = "source scripts/new/common.sh; check_gateway_metrics"

    for metrics_case in ["empty", "generic"]:
        result = subprocess.run(
            ["bash", "-c", command],
            text=True,
            capture_output=True,
            env={**env, "METRICS_CASE": metrics_case},
            check=False,
        )
        assert result.returncode == 1, result.stdout + result.stderr

    for metrics_case in ["gateway", "protected"]:
        result = subprocess.run(
            ["bash", "-c", command],
            text=True,
            capture_output=True,
            env={**env, "METRICS_CASE": metrics_case},
            check=False,
        )
        assert result.returncode == 0, result.stdout + result.stderr


def test_runtime_checks_microservice_readiness_not_only_liveness() -> None:
    common = Path("scripts/new/common.sh").read_text()

    assert "http://127.0.0.1:8092/health/ready" in common
    assert "http://127.0.0.1:8094/health/ready" in common
    assert "127.0.0.1:8765" not in common
    assert 'http://127.0.0.1:8092/health" &>/dev/null' not in common
    assert 'http://127.0.0.1:8094/health" &>/dev/null' not in common
    assert "assert_agent_runtime_image_locked()" in common
    assert "--require-artifact agent_runtime" in common
    assert 'docker image inspect "$task_image" --format \'{{.Id}}\'' in common


def test_migrate_auto_propagates_authority_failure(tmp_path: Path) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    (tmp_path / "database" / "migrations").mkdir(parents=True)

    migrate_script = script_dir / "migrate.sh"
    migrate_script.write_text(Path("scripts/new/migrate.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'log_info() { echo "[INFO] $1"; }\n'
        'log_success() { echo "[OK] $1"; }\n'
        'log_warn() { echo "[WARN] $1"; }\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'log_step() { echo "=> $1"; }\n'
        "load_env() { :; }\n"
        "acquire_migration_advisory_lock() { :; }\n"
        "release_migration_advisory_lock() { :; }\n"
        "run_sql() {\n"
        '  case "$1" in\n'
        "    *\"column_name = 'filename'\"*) echo filename ;;\n"
        "    *\"to_regclass('public.datasets')\"*) echo present ;;\n"
        "  esac\n"
        "}\n"
        "run_sql_file() {\n"
        '  case "$1" in\n'
        '    *001_bad.sql) echo "ERROR: simulated migration failure"; return 1 ;;\n'
        '    *) echo "CREATE TABLE" ;;\n'
        "  esac\n"
        "}\n"
    )
    (tmp_path / "database" / "migrations" / "001_bad.sql").write_text("SELECT broken;\n")

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    uv_log = tmp_path / "uv.log"
    fake_uv = fake_bin / "uv"
    fake_uv.write_text('#!/bin/bash\nprintf \'%s\\n\' "$*" > "$UV_LOG"\nexit 7\n')
    fake_uv.chmod(0o755)

    result = subprocess.run(
        ["bash", str(migrate_script), "--auto"],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "UV_LOG": str(uv_log),
        },
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 7, output
    assert (
        uv_log.read_text().strip()
        == "run --extra database python -m database.authority migrate"
    )
    assert "schema_migrations" not in output


def test_migrate_status_delegates_without_running_schema_files(
    tmp_path: Path,
) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    (tmp_path / "database" / "migrations").mkdir(parents=True)
    (tmp_path / "database" / "schema.sql").write_text("CREATE TABLE datasets;\n")
    (tmp_path / "database" / "migrations" / "002_next.sql").write_text("SELECT 1;\n")
    run_sql_file_log = tmp_path / "run-sql-file.log"

    migrate_script = script_dir / "migrate.sh"
    migrate_script.write_text(Path("scripts/new/migrate.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'log_info() { echo "[INFO] $1"; }\n'
        'log_success() { echo "[OK] $1"; }\n'
        'log_warn() { echo "[WARN] $1"; }\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'log_step() { echo "=> $1"; }\n'
        "load_env() { :; }\n"
        "acquire_migration_advisory_lock() { :; }\n"
        "release_migration_advisory_lock() { :; }\n"
        "run_sql() {\n"
        '  case "$1" in\n'
        "    *\"column_name = 'filename'\"*) echo filename ;;\n"
        "    *\"to_regclass('public.datasets')\"*) echo missing ;;\n"
        "  esac\n"
        "}\n"
        "run_sql_file() {\n"
        '  printf \'%s\\n\' "$1" >> "$RUN_SQL_FILE_LOG"\n'
        '  echo "CREATE TABLE"\n'
        "}\n"
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    uv_log = tmp_path / "uv.log"
    fake_uv = fake_bin / "uv"
    fake_uv.write_text('#!/bin/bash\nprintf \'%s\\n\' "$*" > "$UV_LOG"\n')
    fake_uv.chmod(0o755)

    result = subprocess.run(
        ["bash", str(migrate_script), "--status"],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "UV_LOG": str(uv_log),
            "RUN_SQL_FILE_LOG": str(run_sql_file_log),
        },
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert (
        uv_log.read_text().strip()
        == "run --extra database python -m database.authority status"
    )
    assert not run_sql_file_log.exists()


def test_migrate_init_delegates_to_frozen_baseline_initializer(
    tmp_path: Path,
) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)
    (tmp_path / "database").mkdir()
    (tmp_path / "database" / "migrations").mkdir()
    (tmp_path / "database" / "schema.sql").write_text("CREATE TABLE datasets;\n")
    (tmp_path / "database" / "migrations" / "100_after_init.sql").write_text("SELECT 1;\n")

    migrate_script = script_dir / "migrate.sh"
    migrate_script.write_text(Path("scripts/new/migrate.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'log_info() { echo "[INFO] $1"; }\n'
        'log_success() { echo "[OK] $1"; }\n'
        'log_warn() { echo "[WARN] $1"; }\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'log_step() { echo "=> $1"; }\n'
        "load_env() { :; }\n"
        "acquire_migration_advisory_lock() { :; }\n"
        "release_migration_advisory_lock() { :; }\n"
        "run_sql() {\n"
        '  case "$1" in\n'
        "    *\"column_name = 'filename'\"*) echo filename ;;\n"
        "  esac\n"
        "}\n"
        "run_sql_file() {\n"
        '  if [ "$1" = "database/schema.sql" ]; then\n'
        "    for i in $(seq 1 200000); do\n"
        '      echo "CREATE TABLE t_$i"\n'
        "    done\n"
        "  else\n"
        '    echo "CREATE TABLE migration_result"\n'
        "  fi\n"
        "}\n"
    )

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    uv_log = tmp_path / "uv.log"
    fake_uv = fake_bin / "uv"
    fake_uv.write_text('#!/bin/bash\nprintf \'%s\\n\' "$*" > "$UV_LOG"\n')
    fake_uv.chmod(0o755)

    result = subprocess.run(
        ["bash", str(migrate_script), "--init"],
        text=True,
        capture_output=True,
        env={
            **os.environ,
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "UV_LOG": str(uv_log),
        },
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert (
        uv_log.read_text().strip()
        == "run --extra database python -m database.authority init-fresh"
    )
    assert "CREATE TABLE" not in output


def test_migration_sql_file_execution_stops_on_first_psql_error() -> None:
    script = Path("scripts/new/common.sh").read_text()

    assert script.count("psql -v ON_ERROR_STOP=1") >= 2
    assert 'docker exec -i "$(pg_container)" psql -v ON_ERROR_STOP=1' in script
    assert 'PGPASSWORD="$(pg_password)" psql -v ON_ERROR_STOP=1' in script


def test_backup_restore_uses_env_file_and_stops_on_first_psql_error() -> None:
    script = Path("scripts/new/backup.sh").read_text()

    assert "--env FILE          Use a specific env file instead of .env" in script
    assert "--backup-dir DIR    Store/list backups in an external directory" in script
    assert "--env)" in script
    assert 'log_error "--env requires a file path"' in script
    assert 'ENV_FILE="$2"; shift 2 ;;' in script
    assert script.index("require_env_file") < script.index(
        "# -- Restore -----------------------------------------------------------------"
    )
    assert script.index("require_env_file") < script.index('mkdir -p "$BACKUP_DIR"')
    assert 'psql -v ON_ERROR_STOP=1 -U "$(pg_user)" -d "$(pg_database)"' in script


def test_backup_defaults_outside_checkout_and_rejects_repository_paths(
    tmp_path: Path,
) -> None:
    env_file = tmp_path / "test.env"
    env_file.write_text("")
    external_state = tmp_path / "state"

    listed = subprocess.run(
        ["bash", "scripts/new/backup.sh", "--list"],
        text=True,
        capture_output=True,
        env={**os.environ, "XDG_STATE_HOME": str(external_state), "AI_PLATFORM_BACKUP_DIR": ""},
        check=False,
    )
    assert listed.returncode == 0, listed.stdout + listed.stderr
    assert not (Path.cwd() / "backups").exists()

    rejected = subprocess.run(
        [
            "bash",
            "scripts/new/backup.sh",
            "--backup-dir",
            str(Path.cwd() / "tmp" / "../backups"),
            "--env",
            str(env_file),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    output = rejected.stdout + rejected.stderr
    assert rejected.returncode == 2, output
    assert "must be outside the source checkout" in output
    assert not (Path.cwd() / "backups").exists()

    checkout_alias = tmp_path / "checkout-alias"
    checkout_alias.symlink_to(Path.cwd(), target_is_directory=True)
    symlink_rejected = subprocess.run(
        [
            "bash",
            "scripts/new/backup.sh",
            "--backup-dir",
            str(checkout_alias / "backups"),
            "--env",
            str(env_file),
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    symlink_output = symlink_rejected.stdout + symlink_rejected.stderr
    assert symlink_rejected.returncode == 2, symlink_output
    assert "must be outside the source checkout" in symlink_output


def test_backup_list_reads_external_directory_from_env_file(tmp_path: Path) -> None:
    external = tmp_path / "private-backups"
    external.mkdir()
    expected = external / "gateway_20260829_000000.sql.gz"
    expected.write_bytes(b"backup")
    env_file = tmp_path / "test.env"
    env_file.write_text(f"AI_PLATFORM_BACKUP_DIR={external}\n")

    result = subprocess.run(
        ["bash", "scripts/new/backup.sh", "--list", "--env", str(env_file)],
        text=True,
        capture_output=True,
        env={
            key: value
            for key, value in os.environ.items()
            if key != "AI_PLATFORM_BACKUP_DIR"
        },
        check=False,
    )
    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert expected.name in output


def test_backup_missing_env_does_not_create_backup_directory(tmp_path: Path) -> None:
    script_dir = tmp_path / "scripts" / "new"
    script_dir.mkdir(parents=True)

    backup_script = script_dir / "backup.sh"
    backup_script.write_text(Path("scripts/new/backup.sh").read_text())
    (script_dir / "common.sh").write_text(
        "#!/bin/bash\n"
        "set -euo pipefail\n"
        'SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}")" && pwd)"\n'
        'PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"\n'
        'DEFAULT_ENV_FILE="$PROJECT_ROOT/.env"\n'
        'ENV_FILE="${ENV_FILE:-$DEFAULT_ENV_FILE}"\n'
        'log_error() { echo "[ERROR] $1"; }\n'
        'require_env_file() { [ -f "$ENV_FILE" ] || { log_error "Env file not found: $ENV_FILE"; exit 1; }; }\n'
        "load_env() { :; }\n"
    )

    result = subprocess.run(
        ["bash", str(backup_script), "--env", str(tmp_path / "missing.env")],
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Env file not found" in output
    assert not (tmp_path / "backups").exists()


def test_backup_rejects_unknown_or_missing_env_option() -> None:
    for args, expected in [
        (["--typo"], "Unknown option: --typo"),
        (["--env"], "--env requires a file path"),
        (["--env", "--list"], "--env requires a file path"),
        (["--backup-dir"], "--backup-dir requires a directory path"),
        (["--backup-dir", "--list"], "--backup-dir requires a directory path"),
    ]:
        result = subprocess.run(
            ["bash", "scripts/new/backup.sh", *args],
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        assert result.returncode == 2, output
        assert expected in output
        assert "Creating database backup" not in output
        assert "Restoring from" not in output


def test_common_helpers_respect_env_file_override() -> None:
    script = Path("scripts/new/common.sh").read_text()

    assert 'ENV_FILE="${ENV_FILE:-$DEFAULT_ENV_FILE}"' in script
    assert 'env_file="$(env_file_path)"' in script
    assert "Env file not found: $env_file" in script


def test_deploy_script_accepts_and_forwards_env_file() -> None:
    script = Path("scripts/new/deploy.sh").read_text()

    assert "--env FILE   Use a specific env file instead of .env" in script
    assert "--env)" in script
    assert 'log_error "--env requires a file path"' in script
    assert 'ENV_FILE="$2"; shift 2 ;;' in script
    assert 'validate-env.sh" --env "$ENV_FILE" --config-only' in script
    assert 'validate-env.sh" --env "$ENV_FILE" --infra-only --config-only' in script
    assert '--env-file "$ENV_FILE"' in script
    assert 'ENV_FILE="$ENV_FILE" "$(dirname "$0")/migrate.sh" --auto' in script
    assert 'validate-env.sh" --env "$ENV_FILE" --runtime' in script
    assert '"${COMPOSE_CMD[@]}" "${COMPOSE_FILES[@]}" --env-file "$ENV_FILE"' in script
    assert "compose ps" in script
    assert "\n$COMPOSE_CMD ps\n" not in script


def test_deploy_rejects_missing_env_option_before_preflight() -> None:
    for args in [["--env"], ["--env", "--app"]]:
        result = subprocess.run(
            ["bash", "scripts/new/deploy.sh", *args],
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        assert result.returncode == 2, output
        assert "--env requires a file path" in output
        assert "Pre-flight checks" not in output


def test_deploy_rejects_missing_explicit_env_file_before_preflight(
    tmp_path: Path,
) -> None:
    missing_env = tmp_path / "missing.env"

    result = subprocess.run(
        [
            "bash",
            "scripts/new/deploy.sh",
            "--env",
            str(missing_env),
            "--app",
            "--no-migrate",
        ],
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Env file not found" in output
    assert "Pre-flight checks" not in output
    assert "Starting services" not in output


def test_validate_env_rejects_missing_env_option_before_validation() -> None:
    for args in [["--env"], ["--env", "--runtime"]]:
        result = subprocess.run(
            ["bash", "scripts/new/validate-env.sh", *args],
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        assert result.returncode == 2, output
        assert "--env requires a file path" in output
        assert "Validating configuration" not in output


def test_migrate_script_accepts_env_file() -> None:
    script = Path("scripts/new/migrate.sh").read_text()

    assert "--env FILE   Use a specific env file instead of .env" in script
    assert "--env)" in script
    assert 'log_error "--env requires a file path"' in script
    assert 'ENV_FILE="$2"' in script
    assert script.index('ENV_FILE="$2"') < script.index("load_env")
    assert "python -m database.authority" in script
    assert "ensure_base_schema" not in script
    assert "run_sql" not in script
    assert "schema_migrations" not in script


def test_migrate_rejects_unknown_or_missing_env_option_before_db_access() -> None:
    for args, expected in [
        (["--typo"], "Unknown option: --typo"),
        (["--env"], "--env requires a file path"),
        (["--env", "--status"], "--env requires a file path"),
    ]:
        result = subprocess.run(
            ["bash", "scripts/new/migrate.sh", *args],
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        assert result.returncode == 2, output
        assert expected in output
        assert "Running database migrations" not in output
        assert "Migration status" not in output


def test_migrate_rejects_missing_explicit_env_file_before_db_access(
    tmp_path: Path,
) -> None:
    missing_env = tmp_path / "missing.env"

    result = subprocess.run(
        ["bash", "scripts/new/migrate.sh", "--env", str(missing_env), "--status"],
        text=True,
        capture_output=True,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Env file not found" in output
    assert "Migration status" not in output
    assert "Applied migrations" not in output
    assert "Pending migrations" not in output


def test_make_targets_forward_env_file() -> None:
    makefile = Path("Makefile").read_text()

    assert "ENV_FILE ?= .env" in makefile
    assert 'validate-env.sh --env "$(ENV_FILE)" --config-only' in makefile
    assert 'validate-env.sh --env "$(ENV_FILE)" --runtime' in makefile
    assert 'init-env.sh --env "$(ENV_FILE)" --if-missing' in makefile
    assert 'deploy.sh --env "$(ENV_FILE)" --pull' in makefile
    assert 'deploy.sh --env "$(ENV_FILE)" --build' in makefile
    assert (
        "BUILD_COMPOSE := $(COMPOSE) -f docker-compose.yml -f docker-compose.build.yml" in makefile
    )
    assert '@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/status.sh' in makefile
    assert '@bash $(SCRIPTS)/migrate.sh --env "$(ENV_FILE)"' in makefile
    assert '@bash $(SCRIPTS)/deploy.sh --env "$(ENV_FILE)" $(ARGS)' in makefile
    assert '$(COMPOSE) --env-file "$(ENV_FILE)" stop' in makefile
    assert '$(COMPOSE) --env-file "$(ENV_FILE)" restart' in makefile
    assert '$(COMPOSE) --env-file "$(ENV_FILE)" logs -f' in makefile
    assert '@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/backup.sh' in makefile
    assert '@ENV_FILE="$(ENV_FILE)" bash $(SCRIPTS)/setup-dev.sh' in makefile


def test_quickstart_runs_migrations_before_runtime_validation() -> None:
    makefile = Path("Makefile").read_text()
    deploy = Path("scripts/new/deploy.sh").read_text()

    quickstart = re.search(
        r"^quickstart:.*?(?=^[a-zA-Z_-]+:)",
        makefile,
        flags=re.MULTILINE | re.DOTALL,
    )
    assert quickstart
    assert quickstart.group(0).index("init-env.sh") < quickstart.group(0).index("deploy.sh")
    assert "--pull" in quickstart.group(0)
    assert "--build" not in quickstart.group(0)

    config_idx = deploy.index('validate-env.sh" --env "$ENV_FILE" --config-only')
    infra_up_idx = deploy.index("compose up -d --remove-orphans $START_SERVICES")
    migrate_idx = deploy.index('migrate.sh" --auto')
    app_up_idx = deploy.index("compose up -d --remove-orphans $FULL_APP_SERVICES")
    runtime_idx = deploy.index('validate-env.sh" --env "$ENV_FILE" --runtime')

    assert config_idx < infra_up_idx < migrate_idx < app_up_idx < runtime_idx


def test_status_script_exits_nonzero_when_health_checks_fail(tmp_path: Path) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    env_file = tmp_path / ".env.status"
    env_file.write_text("POSTGRES_PASSWORD=test\nREDIS_PASSWORD=test\n")

    fake_docker = fake_bin / "docker"
    fake_docker.write_text(
        "#!/bin/sh\n"
        'if [ "$1" = "compose" ]; then\n'
        '  if [ "$2" = "version" ]; then exit 0; fi\n'
        '  if [ "$2" = "--env-file" ]; then echo "compose ps"; exit 0; fi\n'
        "fi\n"
        "exit 1\n"
    )
    fake_docker.chmod(fake_docker.stat().st_mode | stat.S_IXUSR)

    fake_curl = fake_bin / "curl"
    fake_curl.write_text("#!/bin/sh\nexit 1\n")
    fake_curl.chmod(fake_curl.stat().st_mode | stat.S_IXUSR)

    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
        "ENV_FILE": str(env_file),
    }
    result = subprocess.run(
        ["bash", "scripts/new/status.sh"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Not available" in output
    assert "health check(s) failed" in output


def test_setup_dev_status_and_help_do_not_require_dev_passwords(tmp_path: Path) -> None:
    fake_bin = _write_fake_runtime_commands(tmp_path)
    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    env.pop("POSTGRES_PASSWORD", None)
    env.pop("REDIS_PASSWORD", None)

    help_result = subprocess.run(
        ["bash", "scripts/new/setup-dev.sh", "--help"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )
    help_output = help_result.stdout + help_result.stderr
    assert help_result.returncode == 0, help_output
    assert "--env FILE" in help_output
    assert "POSTGRES_PASSWORD is required" not in help_output
    assert "REDIS_PASSWORD is required" not in help_output

    status_result = subprocess.run(
        ["bash", "scripts/new/setup-dev.sh", "--status"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )
    status_output = status_result.stdout + status_result.stderr
    assert status_result.returncode == 0, status_output
    assert "Container Status:" in status_output
    assert "Connection Info:" in status_output
    assert "POSTGRES_PASSWORD is required" not in status_output
    assert "REDIS_PASSWORD is required" not in status_output


def test_setup_dev_env_file_and_qdrant_http_port_for_status(tmp_path: Path) -> None:
    fake_bin = _write_fake_runtime_commands(tmp_path)
    env_file = tmp_path / ".env.dev"
    env_file.write_text("QDRANT_HTTP_PORT=7666\nQDRANT_GRPC_PORT=7667\n")
    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    env.pop("POSTGRES_PASSWORD", None)
    env.pop("REDIS_PASSWORD", None)

    result = subprocess.run(
        ["bash", "scripts/new/setup-dev.sh", "--env", str(env_file), "--status"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "Qdrant:     http://127.0.0.1:7666" in output
    assert "POSTGRES_PASSWORD is required" not in output
    assert "REDIS_PASSWORD is required" not in output


def test_setup_dev_rejects_missing_env_option_before_docker_or_secrets() -> None:
    for args in [["--env"], ["--env", "--status"]]:
        result = subprocess.run(
            ["bash", "scripts/new/setup-dev.sh", *args],
            text=True,
            capture_output=True,
            check=False,
        )

        output = result.stdout + result.stderr
        assert result.returncode == 2, output
        assert "--env requires a file path" in output
        assert "Docker daemon is not running" not in output
        assert "POSTGRES_PASSWORD is required" not in output
        assert "REDIS_PASSWORD is required" not in output


def test_setup_dev_rejects_missing_explicit_env_file_before_action(tmp_path: Path) -> None:
    fake_bin = _write_fake_runtime_commands(tmp_path)
    missing_env = tmp_path / "missing.env"
    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    env.pop("POSTGRES_PASSWORD", None)
    env.pop("REDIS_PASSWORD", None)

    result = subprocess.run(
        ["bash", "scripts/new/setup-dev.sh", "--env", str(missing_env), "--status"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "Env file not found" in output
    assert "Container Status:" not in output
    assert "POSTGRES_PASSWORD is required" not in output
    assert "REDIS_PASSWORD is required" not in output


def test_setup_dev_start_still_requires_dev_passwords(tmp_path: Path) -> None:
    fake_bin = _write_fake_runtime_commands(tmp_path)
    env_file = tmp_path / "empty.env"
    env_file.write_text("", encoding="utf-8")
    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
    }
    env.pop("POSTGRES_PASSWORD", None)
    env.pop("REDIS_PASSWORD", None)

    result = subprocess.run(
        ["bash", "scripts/new/setup-dev.sh", "--env", str(env_file), "--start"],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 1, output
    assert "POSTGRES_PASSWORD is required" in output
    assert "REDIS_PASSWORD is required" in output
    assert "Starting dev containers" not in output
