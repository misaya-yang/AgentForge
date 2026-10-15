from __future__ import annotations

import os
import re
import stat
import subprocess
from pathlib import Path


def test_knowledge_worker_is_first_class_in_runtime_scripts() -> None:
    common = Path("scripts/new/common.sh").read_text()
    deploy = Path("scripts/new/deploy.sh").read_text()
    hot_update = Path("scripts/new/hot-update.sh").read_text()
    status = Path("scripts/new/status.sh").read_text()

    assert "knowledge_worker_container()" in common
    assert "compose_running_service_containers knowledge-worker" in common
    assert "check_knowledge_worker_health()" in common
    assert 'FULL_APP_SERVICES="$(topology_service_ids app)"' in deploy
    full_app_services = subprocess.check_output(
        [
            "python3",
            "scripts/deploy/topology_modes.py",
            "--mode",
            "full",
            "--service-ids",
            "app",
        ],
        text=True,
    ).split()
    assert "knowledge-worker" in full_app_services
    assert 'SERVICES="$FULL_APP_SERVICES"' in deploy
    assert 'wait_for_healthy "Knowledge worker" "check_knowledge_worker_health"' in deploy
    assert (
        'wait_for_healthy "Knowledge worker" "check_knowledge_worker_health" 60 '
        '|| fail "Knowledge worker runtime check failed."'
        in Path("scripts/new/validate-env.sh").read_text()
    )
    assert 'knowledge_worker="$(knowledge_worker_container)"' in hot_update
    assert (
        '"$gateway" "/app/src/core/data/platform_catalog_v1.json" "appuser:appuser"'
        in hot_update
    )
    assert 'restart_services+=("knowledge-service" "knowledge-worker")' in hot_update
    assert hot_update.count('copy_dir "apps/knowledge-service/src/knowledge_service"') >= 2
    assert 'wait_for_healthy "Knowledge worker" "check_knowledge_worker_health"' in hot_update
    assert "current_runtime_image" in hot_update
    init_env = Path("scripts/new/init-env.sh").read_text()
    assert "agent_capability_worker_image_tag()" in common
    assert 'existing_worker_image="$(awk -F=' in init_env
    assert 'AGENT_CAPABILITY_WORKER_IMAGE=%s\\n' in init_env
    assert 'assert_runtime_release_unit_locked "$desired_runtime_image" "$desired_worker_image"' in hot_update
    assert "--force-recreate agent-runtime agent-capability-worker" in hot_update
    assert "{{.Image}}" in hot_update and "desired_worker_digest" in hot_update
    assert 'wait_for_healthy "Agent Runtime" "check_agent_runtime_health"' in hot_update
    assert 'check_and_report "Knowledge worker" check_knowledge_worker_health' in status
    makefile = Path("Makefile").read_text(encoding="utf-8")
    assert re.search(
        r"dev-compose:.*?knowledge-service knowledge-worker .*?frontend", makefile, re.S
    )
    assert re.search(
        r"dev-compose-logs:.*?knowledge-service knowledge-worker .*?frontend",
        makefile,
        re.S,
    )


def test_make_deploy_targets_forward_args() -> None:
    makefile = Path("Makefile").read_text()
    for target in ["deploy", "deploy-build", "deploy-cn", "deploy-infra", "deploy-app"]:
        block = re.search(
            rf"^{re.escape(target)}:.*?(?=^[A-Za-z0-9_.%-]+:|\Z)",
            makefile,
            re.MULTILINE | re.DOTALL,
        )
        assert block, f"{target} target is missing"
        logical_recipe = block.group(0).replace("\\\n", " ")
        deploy = re.search(r"bash \$\(SCRIPTS\)/deploy\.sh[^\n]*", logical_recipe)
        assert deploy and "$(ARGS)" in deploy.group(0), f"{target} does not pass ARGS"


def test_status_script_uses_selected_env_file_for_compose_ps(tmp_path: Path) -> None:
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    call_log = tmp_path / "docker-calls.log"
    env_file = tmp_path / ".env.status"
    env_file.write_text("")

    fake_docker = fake_bin / "docker"
    fake_docker.write_text(
        "#!/bin/sh\n"
        'printf \'%s\\n\' "$*" >> "$DOCKER_CALL_LOG"\n'
        'if [ "$1" = "compose" ]; then\n'
        '  if [ "$2" = "version" ]; then exit 0; fi\n'
        '  case " $* " in *" --env-file $EXPECTED_ENV_FILE ps ") '
        '    echo "compose ps used selected env file"; exit 0 ;; esac\n'
        "fi\n"
        'if [ "$1" = "ps" ]; then\n'
        '  case "$*" in\n'
        '    *"com.docker.compose.service=gateway"*) echo gateway-1 ;;\n'
        '    *"com.docker.compose.service=agent-runtime"*) echo agent-runtime-1 ;;\n'
        '    *"com.docker.compose.service=knowledge-worker"*) echo knowledge-worker-1 ;;\n'
        '    *"com.docker.compose.service=agent-capability-worker"*) echo capability-worker-1 ;;\n'
        "  esac\n"
        "  exit 0\n"
        "fi\n"
        'if [ "$1" = "exec" ]; then exit 0; fi\n'
        'if [ "$1" = "inspect" ]; then echo healthy; exit 0; fi\n'
        "exit 1\n"
    )
    fake_docker.chmod(fake_docker.stat().st_mode | stat.S_IXUSR)

    fake_curl = fake_bin / "curl"
    fake_curl.write_text(
        "#!/bin/sh\n"
        'case "$*" in\n'
        "  *\"/metrics\"*) echo '# HELP gateway_up Gateway metrics endpoint availability'; echo 'gateway_up 1'; exit 0 ;;\n"
        "  *) exit 0 ;;\n"
        "esac\n"
    )
    fake_curl.chmod(fake_curl.stat().st_mode | stat.S_IXUSR)

    env = {
        **os.environ,
        "PATH": f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}",
        "DOCKER_CALL_LOG": str(call_log),
        "EXPECTED_ENV_FILE": str(env_file),
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
    assert result.returncode == 0, output
    assert "compose ps used selected env file" in output
    assert "Gateway metrics:" in output
    assert f"--env-file {env_file} ps" in call_log.read_text()
