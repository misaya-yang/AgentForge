import re
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
KNOWLEDGE_DIR = ROOT / "apps/knowledge-service"

LOCK_EXPORTS = (
    (
        KNOWLEDGE_DIR / "build-requirements.lock.txt",
        (
            "--package",
            "knowledge-service",
            "--only-group",
            "image-build",
            "--no-annotate",
            "--no-header",
            "--no-emit-project",
            "--no-emit-workspace",
        ),
    ),
    (
        KNOWLEDGE_DIR / "requirements.lock.txt",
        (
            "--package",
            "knowledge-service",
            "--no-dev",
            "--no-group",
            "image-build",
            "--no-annotate",
            "--no-header",
            "--no-emit-package",
            "knowledge-service",
            "--no-emit-package",
            "ai-gateway-core",
            "--no-emit-package",
            "ai-gateway-contracts",
        ),
    ),
)


def _logical_requirements(text: str) -> list[str]:
    requirements: list[str] = []
    current: list[str] = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        continued = line.endswith("\\")
        current.append(line.removesuffix("\\").strip())
        if not continued:
            requirements.append(" ".join(current))
            current = []
    assert not current, "lock file ends with an incomplete requirement"
    return requirements


def _without_comments(text: str) -> str:
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


@pytest.mark.parametrize(("checked_in", "export_args"), LOCK_EXPORTS)
def test_knowledge_image_locks_match_uv_lock(
    tmp_path: Path,
    checked_in: Path,
    export_args: tuple[str, ...],
) -> None:
    generated = tmp_path / checked_in.name
    result = subprocess.run(
        [
            "uv",
            "export",
            "--locked",
            "--quiet",
            *export_args,
            "--output-file",
            str(generated),
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert checked_in.read_bytes() == generated.read_bytes(), (
        f"{checked_in.relative_to(ROOT)} is stale; run make kb-image-lock-refresh"
    )


@pytest.mark.parametrize("lock_path", [lock_path for lock_path, _args in LOCK_EXPORTS])
def test_knowledge_image_locks_use_exact_versions_and_sha256_hashes(
    lock_path: Path,
) -> None:
    requirements = _logical_requirements(lock_path.read_text())

    assert requirements
    for requirement in requirements:
        assert re.match(r"^[A-Za-z0-9][A-Za-z0-9._-]*==[^ ;]+", requirement), requirement
        hashes = re.findall(r"--hash=sha256:([0-9a-f]{64})(?:\s|$)", requirement)
        assert hashes, requirement
        assert len(hashes) == requirement.count("--hash=sha256:"), requirement


def test_knowledge_dockerfile_has_no_floating_python_installs() -> None:
    dockerfile = _without_comments((KNOWLEDGE_DIR / "Dockerfile").read_text())

    assert "pip install --upgrade" not in dockerfile
    assert dockerfile.count("--require-hashes") == 2
    assert dockerfile.count("--no-build-isolation") == 2
    assert "python -m pip install --no-deps --no-build-isolation" in dockerfile
    assert "build-requirements.lock.txt" in dockerfile
    assert "requirements.lock.txt" in dockerfile
    assert "COPY packages/ai-gateway-contracts/ /build/packages/ai-gateway-contracts/" in dockerfile
    install_start = dockerfile.index("python -m pip install --no-deps --no-build-isolation")
    install_end = dockerfile.index("--index-url", install_start)
    local_install = dockerfile[install_start:install_end]
    contracts_position = local_install.index("/build/packages/ai-gateway-contracts")
    core_position = local_install.index("/build/packages/ai-gateway-core")
    service_position = local_install.index("/build/knowledge-service")
    assert contracts_position < core_position < service_position
    assert "hatchling==1.27.0" in (KNOWLEDGE_DIR / "build-requirements.lock.txt").read_text()


def test_gateway_dockerfile_installs_local_contracts_before_core() -> None:
    dockerfile = _without_comments((ROOT / "Dockerfile").read_text())

    contracts_copy = "COPY packages/ai-gateway-contracts/ ./packages/ai-gateway-contracts/"
    core_copy = "COPY packages/ai-gateway-core/ ./packages/ai-gateway-core/"
    local_install = "pip install ./packages/ai-gateway-contracts ./packages/ai-gateway-core"
    assert contracts_copy in dockerfile
    assert core_copy in dockerfile
    assert local_install in dockerfile
    assert dockerfile.index(contracts_copy) < dockerfile.index(core_copy)
    assert dockerfile.index(core_copy) < dockerfile.index(local_install)


def test_development_compose_mounts_contracts_for_python_consumers() -> None:
    compose = _without_comments((ROOT / "docker-compose.dev.yml").read_text())
    contracts_mount = (
        "./packages/ai-gateway-contracts/src/ai_gateway_contracts:"
        "/opt/venv/lib/python3.12/site-packages/ai_gateway_contracts"
    )

    assert compose.count(contracts_mount) == 3


def test_knowledge_package_declares_workspace_contracts_dependency() -> None:
    project = _without_comments((KNOWLEDGE_DIR / "pyproject.toml").read_text())
    dependencies = project.split("dependencies = [", 1)[1].split("]", 1)[0]
    sources = project.split("[tool.uv.sources]", 1)[1].split("[", 1)[0]

    assert '"ai-gateway-contracts",' in dependencies
    assert "ai-gateway-contracts = { workspace = true }" in sources


def test_knowledge_image_gate_runs_in_pinned_ci_and_keeps_multiarch_publish() -> None:
    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    publish = (ROOT / ".github/workflows/docker-publish.yml").read_text()

    assert 'UV_VERSION: "0.13.0"' in ci
    uv_installs = re.findall(r"- name: Install uv\n\s+run: ([^\n]+)", ci)
    assert len(uv_installs) >= 3
    assert set(uv_installs) == {'python -m pip install "uv==${UV_VERSION}"'}
    assert "run: make kb-image-lock-gate" in ci
    assert "dockerfile: ./apps/knowledge-service/Dockerfile" in publish
    assert "platforms: linux/amd64,linux/arm64" in publish
