from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path

SCRIPT_PATH = Path("web/docker-entrypoint.d/40-runtime-config.sh")
NGINX_CONFIG_PATH = Path("web/nginx.conf")
COMPOSE_PATH = Path("docker-compose.yml")


def _run_entrypoint(tmp_path: Path, extra_env: dict[str, str] | None = None) -> str:
    output_path = tmp_path / "runtime-config.js"
    env = {
        **os.environ,
        "RUNTIME_CONFIG_OUTPUT_PATH": str(output_path),
    }
    if extra_env:
        env.update(extra_env)

    result = subprocess.run(
        ["sh", str(SCRIPT_PATH)],
        text=True,
        capture_output=True,
        env=env,
        check=False,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert output_path.exists()
    return output_path.read_text()


def _runtime_values(script_text: str) -> dict[str, str]:
    values: dict[str, str] = {}
    for key, raw_value in re.findall(r'^\s+(\w+): "(.*)"[,]?$', script_text, re.MULTILINE):
        values[key] = json.loads(f'"{raw_value}"')
    return values


def test_runtime_config_entrypoint_writes_safe_defaults(tmp_path: Path) -> None:
    script_text = _run_entrypoint(tmp_path)

    values = _runtime_values(script_text)

    assert values["apiUrl"] == ""
    assert values["apiBaseUrl"] == ""
    assert values["authEmailDomain"] == "example.com"
    assert values["supportEmail"] == "admin@example.com"
    assert values["telemetryEndpoint"] == ""
    assert values["sseDebug"] == ""
    assert values["agentStudioEnabled"] == "true"


def test_runtime_config_entrypoint_defaults_support_email_to_auth_domain(
    tmp_path: Path,
) -> None:
    script_text = _run_entrypoint(
        tmp_path,
        {
            "VITE_AUTH_EMAIL_DOMAIN": "myapp.test",
        },
    )

    values = _runtime_values(script_text)

    assert values["authEmailDomain"] == "myapp.test"
    assert values["supportEmail"] == "admin@myapp.test"


def test_runtime_config_entrypoint_escapes_js_string_values(tmp_path: Path) -> None:
    script_text = _run_entrypoint(
        tmp_path,
        {
            "VITE_API_URL": 'https://api.example.com/"gateway"\\v1',
            "VITE_API_BASE_URL": "https://gateway.example.com/api\nv1",
            "VITE_AUTH_EMAIL_DOMAIN": "myapp.test",
            "VITE_SUPPORT_EMAIL": r"support\desk@example.com",
            "VITE_TELEMETRY_ENDPOINT": 'https://telemetry.example.com/collect?tag="release"',
            "VITE_SSE_DEBUG": "true\r\nignored",
            "VITE_AGENT_STUDIO_ENABLED": "false",
        },
    )

    values = _runtime_values(script_text)

    assert values["apiUrl"] == 'https://api.example.com/"gateway"\\v1'
    assert values["apiBaseUrl"] == "https://gateway.example.com/api v1"
    assert values["authEmailDomain"] == "myapp.test"
    assert values["supportEmail"] == r"support\desk@example.com"
    assert values["telemetryEndpoint"] == 'https://telemetry.example.com/collect?tag="release"'
    assert values["sseDebug"] == "true  ignored"
    assert values["agentStudioEnabled"] == "false"


def test_nginx_does_not_cache_runtime_config() -> None:
    nginx_config = NGINX_CONFIG_PATH.read_text()
    match = re.search(
        r"location = /runtime-config\.js \{\n(?P<body>.*?)\n        \}",
        nginx_config,
        flags=re.DOTALL,
    )

    assert match, "runtime-config.js must have an exact nginx location"
    body = match.group("body")
    assert 'add_header Cache-Control "no-store, no-cache, must-revalidate" always;' in body
    assert 'add_header Pragma "no-cache" always;' in body
    assert "try_files $uri =404;" in body


def test_compose_passes_agent_studio_flag_to_frontend_runtime() -> None:
    compose_text = COMPOSE_PATH.read_text()
    match = re.search(
        r"^  frontend:\n(?P<body>.*?)(?=^  [a-zA-Z0-9_-]+:\n)",
        compose_text,
        flags=re.DOTALL | re.MULTILINE,
    )

    assert match, "docker-compose.yml must define the frontend service"
    assert (
        'VITE_AGENT_STUDIO_ENABLED: "${VITE_AGENT_STUDIO_ENABLED:-true}"'
        in match.group("body")
    )
