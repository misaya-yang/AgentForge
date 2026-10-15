"""Focused regression tests for ARC-00A/ARC-04 inventory trust boundaries."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "inventory"))
sys.path.insert(0, str(ROOT / "scripts" / "core_boundary"))

import check_core_boundary as core_gate  # noqa: E402
import inventory_core_consumption as core_inventory  # noqa: E402
from _common import (  # noqa: E402
    BaselineProvenanceError,
    clean_git_head,
    require_payload_revision,
    require_source_tree,
)
from check_core_boundary import (  # noqa: E402
    check_contracts_imports,
    check_mixed_export_consumers_no_growth,
    check_mixed_export_map_consistency,
    check_shim_consumers_no_growth,
)
from inventory_core_consumption import (  # noqa: E402
    CONTRACTS_PKG_DIR,
    CORE_PKG_DIR,
    MIXED_CORE_EXPORTS,
    InventoryProvenanceError,
    build_inventory,
    build_inventory_from_git,
    clean_source_revision,
    verify_inventory_provenance,
)


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def _clean_repo(tmp_path: Path) -> tuple[Path, str]:
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "inventory-test@example.invalid")
    _git(root, "config", "user.name", "Inventory Test")
    (root / "tracked.txt").write_text("settled\n", encoding="utf-8")
    _git(root, "add", "tracked.txt")
    _git(root, "commit", "-q", "-m", "settled")
    return root, _git(root, "rev-parse", "HEAD")


def test_clean_git_head_binds_the_exact_settled_revision(tmp_path: Path) -> None:
    root, head = _clean_repo(tmp_path)

    assert clean_git_head(root) == head
    assert clean_git_head(root, expected_sha=head) == head
    require_payload_revision({"base_git_sha": head}, head, name="baseline.json")

    with pytest.raises(BaselineProvenanceError, match="declares base_git_sha"):
        require_payload_revision(
            {"base_git_sha": "0" * 40},
            head,
            name="baseline.json",
        )


@pytest.mark.parametrize("kind", ["tracked", "untracked"])
def test_clean_git_head_rejects_dirty_or_untracked_facts(
    tmp_path: Path,
    kind: str,
) -> None:
    root, _head = _clean_repo(tmp_path)
    if kind == "tracked":
        (root / "tracked.txt").write_text("not committed\n", encoding="utf-8")
    else:
        (root / "untracked.txt").write_text("not committed\n", encoding="utf-8")

    with pytest.raises(BaselineProvenanceError, match="clean working tree"):
        clean_git_head(root)


def test_source_revision_allows_only_a_committed_baseline_artifact(tmp_path: Path) -> None:
    root, source_sha = _clean_repo(tmp_path)
    output = root / "baselines"
    output.mkdir()
    (output / "facts.json").write_text("{}\n", encoding="utf-8")
    _git(root, "add", "baselines/facts.json")
    _git(root, "commit", "-q", "-m", "record baseline")

    require_source_tree(source_sha, root, excluded_paths=(output,))

    (root / "runbook.md").write_text("receipt only\n", encoding="utf-8")
    _git(root, "add", "runbook.md")
    _git(root, "commit", "-q", "-m", "record receipt")
    require_source_tree(
        source_sha,
        root,
        included_paths=(root / "tracked.txt",),
    )

    (root / "tracked.txt").write_text("changed facts\n", encoding="utf-8")
    _git(root, "add", "tracked.txt")
    _git(root, "commit", "-q", "-m", "change facts")
    with pytest.raises(BaselineProvenanceError, match="facts differ"):
        require_source_tree(
            source_sha,
            root,
            included_paths=(root / "tracked.txt",),
        )


def test_core_inventory_v3_rebuilds_declared_git_object(tmp_path: Path) -> None:
    root, source_sha = _clean_repo(tmp_path)

    inventory = build_inventory_from_git(root, source_sha)
    provenance = verify_inventory_provenance(root, inventory)

    assert inventory["schema_version"] == "arc04-core-inventory/v3"
    assert inventory["base_sha"] == source_sha
    assert provenance["source_commit"] == source_sha

    inventory["gateway_core_module_count"] = 1
    with pytest.raises(InventoryProvenanceError, match="does not match"):
        verify_inventory_provenance(root, inventory)


def test_core_inventory_formal_flow_rejects_dirty_checkout(tmp_path: Path) -> None:
    root, source_sha = _clean_repo(tmp_path)
    assert clean_source_revision(root, source_sha) == source_sha

    (root / "tracked.txt").write_text("dirty facts\n", encoding="utf-8")
    with pytest.raises(InventoryProvenanceError, match="clean working tree"):
        clean_source_revision(root, source_sha)

    # An explicit Git-object build remains bound to the committed content and
    # never incorporates the dirty file bytes.
    inventory = build_inventory_from_git(root, source_sha)
    assert verify_inventory_provenance(root, inventory)["source_commit"] == source_sha


def test_core_inventory_write_requires_separate_committed_review(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root, source_sha = _clean_repo(tmp_path)
    output = "reports/inventory/core-import-inventory.json"
    monkeypatch.setattr(core_inventory, "repo_root", lambda: root)

    assert core_inventory.main(
        ["--write", "--source-rev", source_sha, "--output", output]
    ) == 0
    # The newly written artifact makes the checkout dirty, so the safe default
    # cannot immediately bless the same command's output.
    assert core_inventory.main(["--output", output]) == 2

    _git(root, "add", output)
    _git(root, "commit", "-q", "-m", "reviewed inventory")
    assert core_inventory.main(["--verify", "--output", output]) == 0


def test_core_gate_provenance_error_writes_failure_evidence(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root, _source_sha = _clean_repo(tmp_path)
    baseline = root / "bad-inventory.json"
    evidence = root / "core-boundary-evidence.json"
    baseline.write_text('{"schema_version": "self-certified"}\n', encoding="utf-8")
    monkeypatch.setattr(core_gate, "repo_root", lambda: root)

    assert (
        core_gate.main(
            [
                "--baseline",
                str(baseline),
                "--evidence-out",
                str(evidence),
            ]
        )
        == 2
    )
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["result"] == "error"
    assert payload["provenance"]["result"] == "fail"


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_facade_consumers_are_attributed_to_the_runtime_shim(tmp_path: Path) -> None:
    root = tmp_path / "fixture"
    facade = "packages/ai-gateway-core/src/ai_gateway_core/agents/__init__.py"
    consumer = "src/runtime_consumer.py"
    _write(root / CORE_PKG_DIR / "__init__.py", "")
    _write(root / CORE_PKG_DIR / "agents" / "__init__.py", "from .runtime import runtime_sha256\n")
    _write(
        root / CORE_PKG_DIR / "agents" / "runtime.py",
        '"""Compatibility shim — implementation moved to ``ai_gateway_contracts``."""\n'
        "from ai_gateway_contracts.agent_runtime import runtime_sha256\n",
    )
    _write(root / consumer, "from ai_gateway_core.agents import runtime_sha256\n")
    _write(root / CONTRACTS_PKG_DIR / "__init__.py", "")
    _write(
        root / CONTRACTS_PKG_DIR / "agent_runtime.py",
        "def runtime_sha256(value): return value\n",
    )

    inventory = build_inventory(root)
    shim = inventory["shim_consumers"]["ai_gateway_core.agents.runtime"]
    assert shim == {"files": [facade, consumer], "count": 2}
    assert inventory["shim_facade_exports"]["ai_gateway_core.agents"] == {
        "runtime_sha256": "ai_gateway_core.agents.runtime"
    }

    violations = check_shim_consumers_no_growth(
        root,
        {
            "shim_consumers": {
                "ai_gateway_core.agents.runtime": {
                    "files": [facade],
                    "count": 1,
                }
            }
        },
    )
    assert any(consumer in violation for violation in violations)


def test_every_shim_identity_contract_is_machine_visible() -> None:
    inventory = build_inventory(ROOT)
    identity_test = "packages/ai-gateway-contracts/tests/test_shim_identity.py"

    assert set(inventory["shim_consumers"]) == set(core_inventory.CORE_TO_CONTRACTS)
    for module, consumers in inventory["shim_consumers"].items():
        assert identity_test in consumers["files"], (
            f"{module} identity coverage is invisible to the consumer ledger"
        )


def test_contracts_stdlib_is_a_pure_computation_allowlist(tmp_path: Path) -> None:
    root = tmp_path / "fixture"
    module = root / CONTRACTS_PKG_DIR / "capability_proof.py"
    _write(root / CONTRACTS_PKG_DIR / "__init__.py", "")
    _write(
        module,
        "from dataclasses import dataclass\n"
        "from typing import Any\n"
        "import hashlib\n"
        "import json\n",
    )
    assert check_contracts_imports(root) == []

    _write(module, "import urllib.request\nopen('forbidden')\n")
    violations = check_contracts_imports(root)
    assert any("forbidden contracts dependency: urllib" in item for item in violations)
    assert any("forbidden contracts I/O call: open" in item for item in violations)


def test_gateway_secret_mixed_exports_have_consumers_and_deletion_ledger(
    tmp_path: Path,
) -> None:
    root = tmp_path / "fixture"
    module = "ai_gateway_core.auth.gateway_secret"
    facade = "packages/ai-gateway-core/src/ai_gateway_core/auth/__init__.py"
    consumer = "src/replay_consumer.py"
    _write(root / CORE_PKG_DIR / "__init__.py", "")
    _write(
        root / CORE_PKG_DIR / "auth" / "gateway_secret.py",
        "from ai_gateway_contracts.replay import InMemoryReplayStore, ReplayStore\n",
    )
    _write(
        root / facade,
        "from .gateway_secret import InMemoryReplayStore, ReplayStore\n",
    )
    _write(root / consumer, "from ai_gateway_core.auth import ReplayStore\n")
    _write(root / CONTRACTS_PKG_DIR / "__init__.py", "")
    _write(
        root / CONTRACTS_PKG_DIR / "replay.py",
        "class InMemoryReplayStore: pass\nclass ReplayStore: pass\n",
    )

    inventory = build_inventory(root)
    mixed = inventory["mixed_export_consumers"][module]
    assert mixed["contracts_module"] == "ai_gateway_contracts.replay"
    assert mixed["deletion_condition"]
    assert mixed["symbols"] == {
        "InMemoryReplayStore": {"files": [facade], "count": 1},
        "ReplayStore": {"files": [facade, consumer], "count": 2},
    }
    assert check_mixed_export_map_consistency(root) == []

    spec = MIXED_CORE_EXPORTS[module]
    baseline = {
        "mixed_export_consumers": {
            module: {
                "contracts_module": spec["contracts_module"],
                "replacement": spec["replacement"],
                "deletion_condition": spec["deletion_condition"],
                "symbols": {
                    "InMemoryReplayStore": {"files": [facade], "count": 1},
                    "ReplayStore": {
                        "files": [facade, "src/retired_consumer.py"],
                        "count": 2,
                    },
                },
            }
        }
    }
    violations = check_mixed_export_consumers_no_growth(root, baseline)
    assert any(consumer in item for item in violations)
