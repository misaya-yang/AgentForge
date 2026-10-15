from __future__ import annotations

import json
import os
import re
import subprocess
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
COMMUNITY_PLUGIN_SOURCE_COMMIT = "0a6e37e4e242c944380228fa29dbd14e64ac1b63"


def _read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def test_bundled_community_agent_plugins_are_pinned_read_only_data() -> None:
    expected_agents = {
        "community-doublecheck": {"./agents/doublecheck.md"},
        "community-engineering-reviewers": {
            "./agents/security-reviewer.md",
            "./agents/system-architecture-reviewer.md",
            "./agents/technical-writer.md",
        },
    }
    expected_files = {
        plugin_name: {
            "LICENSE",
            "THIRD_PARTY_NOTICES.md",
            "plugin.json",
            *(agent_path.removeprefix("./") for agent_path in agent_paths),
        }
        for plugin_name, agent_paths in expected_agents.items()
    }

    for plugin_name, agent_paths in expected_agents.items():
        plugin_root = ROOT / "agent-plugins" / plugin_name
        manifest = json.loads((plugin_root / "plugin.json").read_text(encoding="utf-8"))
        assert manifest["$schema"] == ("https://agent-plugins.org/schemas/1.0.0/plugin.schema.json")
        assert manifest["name"] == plugin_name
        assert manifest["license"] == "MIT"
        extension = manifest["extensions"]["com.misaya.ai-gateway"]
        assert set(extension) == {"agents"}
        assert set(extension["agents"]) == agent_paths

        bundled_files = {
            str(path.relative_to(plugin_root)) for path in plugin_root.rglob("*") if path.is_file()
        }
        assert bundled_files == expected_files[plugin_name]
        assert not (plugin_root / "mcp.json").exists()
        assert not (plugin_root / "scripts").exists()
        assert not (plugin_root / "hooks").exists()

        notice = (plugin_root / "THIRD_PARTY_NOTICES.md").read_text(encoding="utf-8")
        license_text = (plugin_root / "LICENSE").read_text(encoding="utf-8")
        assert COMMUNITY_PLUGIN_SOURCE_COMMIT in notice
        assert "https://github.com/github/awesome-copilot" in notice
        assert license_text.startswith("MIT License\n\nCopyright GitHub, Inc.")

        for agent_path in sorted(agent_paths):
            resolved = (plugin_root / agent_path).resolve(strict=True)
            assert resolved.is_relative_to(plugin_root.resolve())
            text = resolved.read_text(encoding="utf-8")
            _, frontmatter_text, instructions = text.split("---", 2)
            frontmatter = yaml.safe_load(frontmatter_text)
            assert set(frontmatter) == {
                "id",
                "name",
                "description",
                "base_type",
                "allowed_tools",
                "allowed_tool_categories",
                "initial_max_turns",
                "initial_max_tool_calls",
                "recommended_max_tokens",
                "initial_timeout_seconds",
                "idle_timeout_seconds",
            }
            assert frontmatter["id"] == resolved.stem
            assert frontmatter["base_type"] == "explore"
            assert frontmatter["allowed_tools"] == []
            assert frontmatter["allowed_tool_categories"] == ["retrieval", "utility"]
            # Package values are an initial/recommended lease, never runtime
            # authority; each stays within the explore host ceilings.
            assert 1 <= frontmatter["initial_max_turns"] <= 16
            assert 1 <= frontmatter["initial_max_tool_calls"] <= 32
            assert frontmatter["recommended_max_tokens"] == 4096
            assert frontmatter["initial_timeout_seconds"] == 120
            assert frontmatter["idle_timeout_seconds"] == 120
            assert "read-only" in instructions.lower()
            for forbidden in (
                "git commit",
                "git push",
                "pull request",
                "installer",
                "run a script",
                "launch a process",
            ):
                assert forbidden not in instructions.lower()

    env_example = _read(".env.example")
    assert "ASSISTANT_AGENT_PLUGIN_PATHS" not in env_example
    assert "ASSISTANT_TRUSTED_AGENT_PLUGINS" not in env_example


def test_compose_uses_gateway_bucket_fallback_and_named_volume_data_dir():
    compose = _read("docker-compose.yml")
    env_example = _read(".env.example")

    assert (
        'GATEWAY_STORAGE__S3__BUCKET: "${GATEWAY_STORAGE__S3__BUCKET:-${S3_BUCKET:-}}"' in compose
    )
    assert (
        'KNOWLEDGE_STORAGE__S3__BUCKET: "${KNOWLEDGE_STORAGE__S3__BUCKET:-${GATEWAY_STORAGE__S3__BUCKET:-${S3_BUCKET:-}}}"'
        in compose
    )
    assert (
        "./apps/knowledge-service/src/knowledge_service:/usr/local/lib/python3.12/site-packages/knowledge_service"
        not in compose
    )
    assert "gateway-data:/app/data" in compose
    assert "gateway-init:" in compose
    assert "service_completed_successfully" in compose
    assert "mkdir -p /app/data/images /app/logs" in compose
    assert "chown -R 1000:1000 /app/data /app/logs" in compose
    assert "x-container-proxy-env:" in compose
    assert 'HTTP_PROXY: "${CONTAINER_HTTP_PROXY:-}"' in compose
    assert "host.docker.internal" in env_example
    assert "CONTAINER_NO_PROXY=" in env_example


def test_compose_isolates_runtime_and_capability_worker_state():
    compose_text = _read("docker-compose.yml")
    compose = yaml.safe_load(compose_text)
    services = compose["services"]
    runtime = services["agent-runtime"]
    worker = services["agent-capability-worker"]
    assert "agent-runtime-home:/var/lib/ai-platform-agent-runtime/runtime-home" in runtime["volumes"]
    assert "agent-capability-workspaces:/workspace" in worker["volumes"]
    assert all("/var/run/docker.sock" not in str(volume) for volume in worker["volumes"])
    assert compose["volumes"]["agent-runtime-home"] == {}
    assert compose["volumes"]["agent-capability-workspaces"] == {}
    assert runtime["environment"]["AI_PLATFORM_CAPABILITY_WORKER_URL"] == (
        "${AI_PLATFORM_CAPABILITY_WORKER_URL:-http://agent-capability-worker:8095}"
    )
    assert "/Users/" not in compose_text


def test_playwright_uses_the_repository_owned_live_stack() -> None:
    config = _read("web/playwright.config.ts")
    script = _read("scripts/dev/start_e2e_stack.sh")

    assert "webServer" not in config
    assert "playwright.live.config.ts" in script
    assert "assistant" + "_service" not in script


def test_deploy_stops_app_services_before_migrations():
    script = _read("scripts/new/deploy.sh")
    common = _read("scripts/new/common.sh")

    assert "assert_compose_owner()" in common
    assert "com.docker.compose.project.working_dir" in common
    assert "Refusing to mutate Docker project 'ai-gateway'" in common
    assert "\nassert_compose_owner\n" in script
    assert "Stopping application services before migrations" in script
    assert "Application services will start after migrations" in script
    assert "migrate.sh" in script and "--auto" in script


def test_compose_owner_guard_rejects_unlabeled_or_foreign_expected_containers(
    tmp_path: Path,
) -> None:
    fake_docker = tmp_path / "docker"
    fake_docker.write_text(
        """#!/bin/bash
if [ "${1:-}" != "inspect" ]; then
    exit 1
fi
container="${!#}"
if [ "$container" != "ai-gateway-backend" ]; then
    exit 1
fi
if [ "${2:-}" != "-f" ]; then
    exit 0
fi
format="${3:-}"
if [[ "$format" == *working_dir* ]]; then
    printf '%s\n' "${FAKE_OWNER-}"
elif [[ "$format" == *compose.project* ]]; then
    printf '%s\n' "${FAKE_PROJECT-}"
elif [[ "$format" == *compose.service* ]]; then
    printf '%s\n' gateway
fi
""",
        encoding="utf-8",
    )
    fake_docker.chmod(0o755)

    def run_guard(*, owner: str, project: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$REPO_ROOT/scripts/new/common.sh"; assert_compose_owner "$REPO_ROOT"',
            ],
            env={
                **os.environ,
                "PATH": f"{tmp_path}:{os.environ['PATH']}",
                "REPO_ROOT": str(ROOT),
                "FAKE_OWNER": owner,
                "FAKE_PROJECT": project,
            },
            text=True,
            capture_output=True,
            check=False,
        )

    assert run_guard(owner=str(ROOT), project="ai-gateway").returncode == 0
    for owner, project in (
        ("", "ai-gateway"),
        (str(ROOT), "foreign-project"),
        (str(tmp_path / "other-checkout"), "ai-gateway"),
    ):
        result = run_guard(owner=owner, project=project)
        assert result.returncode != 0, result.stdout + result.stderr
        assert "Refusing to mutate Docker project" in result.stdout + result.stderr

    doctor = _read("scripts/new/doctor.sh")
    assert 'if [ -z "$owner" ]; then' in doctor
    assert 'elif [ "$owner" != "$PROJECT_ROOT" ]; then' in doctor
    assert 'elif [ "$project" != "ai-gateway" ]; then' in doctor


def test_migrate_shell_has_no_legacy_ledger_or_sql_writer():
    script = _read("scripts/new/migrate.sh")

    assert "python -m database.authority" in script
    assert "INSERT INTO public.schema_migrations" not in script
    assert "CREATE TABLE IF NOT EXISTS" not in script
    assert "run_sql" not in script
    assert not re.search(r"^\s*psql\b", script, re.MULTILINE)


def test_migrate_shell_does_not_reimplement_legacy_aliases() -> None:
    script = _read("scripts/new/migrate.sh")

    assert "legacy_filename_alias" not in script
    assert "089_codex_runtime_thread_store.sql" not in script
    assert "094_codex_runtime_legacy_import_normalization.sql" not in script


def test_deploy_builds_and_pins_the_agent_runtime_image() -> None:
    deploy = _read("scripts/new/deploy.sh")
    compose = _read("docker-compose.yml")
    example = _read(".env.example")
    receipt = json.loads(_read("deploy/agent-runtime-source/source-receipt.json"))
    expected_image = (
        "ai-gateway-agent-runtime:local-"
        f"{receipt['source']['upstream_sha'][:12]}-"
        f"{receipt['overlay']['sha256'][:12]}"
    )
    expected_worker_image = (
        "ai-gateway-agent-capability-worker:local-"
        f"{receipt['source']['upstream_sha'][:12]}-"
        f"{receipt['overlay']['sha256'][:12]}"
    )
    expected_revision = (
        f"{receipt['source']['upstream_sha']}+{receipt['overlay']['sha256'][:12]}"
    )

    assert "agent_runtime_kernel_revision" in deploy
    assert "agent_runtime_image_tag" in deploy
    assert "build_agent_runtime_image.sh" in deploy
    assert "AI_PLATFORM_AGENT_RUNTIME_SOURCE is required for --build" in deploy
    assert 'AI_PLATFORM_AGENT_RUNTIME_IMAGE_TAG' in deploy
    assert "assert_agent_runtime_image_locked" in deploy
    assert f"AI_PLATFORM_AGENT_RUNTIME_IMAGE:-{expected_image}" in compose
    assert f"AI_PLATFORM_AGENT_RUNTIME_IMAGE={expected_image}" in example
    assert f"AGENT_CAPABILITY_WORKER_IMAGE:-{expected_worker_image}" in compose
    assert f"AGENT_CAPABILITY_WORKER_IMAGE={expected_worker_image}" in example
    assert f"AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION={expected_revision}" in example


def test_cli_publish_packs_from_the_package_directory() -> None:
    publish = _read(".github/workflows/publish-sdk.yml")
    package = json.loads(_read("sdk/cli/package.json"))

    assert "cd sdk/cli\n          npm pack --dry-run" in publish
    assert "npm --prefix sdk/cli pack --dry-run" not in publish
    prepare = "node sdk/cli/scripts/prepare-native-vendor.mjs linux-x64"
    build = "scripts/harness/build_agent_runtime_cli_package.sh"
    assert prepare in publish
    assert publish.index(prepare) < publish.index(build)
    assert package["os"] == ["linux"]
    assert package["cpu"] == ["x64"]


def test_common_load_env_preserves_explicit_runtime_image_and_reads_defaults(tmp_path: Path) -> None:
    env_file = tmp_path / "runtime.env"
    env_file.write_text(
        "AI_PLATFORM_AGENT_RUNTIME_IMAGE=ai-gateway-agent-runtime:stale-file-value\n"
        "RUNTIME_DEFAULT_FROM_FILE=from-file\n",
        encoding="utf-8",
    )
    command = (
        "source scripts/new/common.sh; "
        f"ENV_FILE={env_file}; "
        "export ENV_FILE AI_PLATFORM_AGENT_RUNTIME_IMAGE=explicit-runtime-value; "
        "load_env; "
        "printf '%s|%s' \"$AI_PLATFORM_AGENT_RUNTIME_IMAGE\" \"$RUNTIME_DEFAULT_FROM_FILE\""
    )
    result = subprocess.run(
        ["bash", "-c", command],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "explicit-runtime-value|from-file"
