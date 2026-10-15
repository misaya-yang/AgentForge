from __future__ import annotations

import json
from pathlib import Path

import pytest

from scripts.harness import agent_runtime_supply_chain as supply_chain
from scripts.harness.agent_runtime_supply_chain import (
    ContractError,
    record_local_image,
    refresh_source_lock,
    sha256_file,
    validate_lock,
)


def _write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True) + "\n", encoding="utf-8")


def _fixture(
    tmp_path: Path,
    *,
    app_server_runnable: bool = True,
    agent_runtime_runnable: bool = True,
    capability_worker_runnable: bool = False,
) -> Path:
    receipt_path = tmp_path / "deploy/agent-runtime-source/source-receipt.json"
    sbom_path = tmp_path / "deploy/agent-runtime-source/sbom.cdx.json"
    capability_sbom_path = tmp_path / "deploy/agent-runtime-source/capability-worker-sbom.cdx.json"
    notice_path = tmp_path / "deploy/agent-runtime-source/NOTICE.md"
    capability_schema_path = tmp_path / "database/migrations/096_agent_capability_executions.sql"
    sbom_path.parent.mkdir(parents=True, exist_ok=True)
    sbom_path.write_text("{}\n", encoding="utf-8")
    capability_sbom_path.write_text("{}\n", encoding="utf-8")
    notice_path.write_text("OpenAI upstream notice\n", encoding="utf-8")
    capability_schema_path.parent.mkdir(parents=True, exist_ok=True)
    capability_schema_path.write_text("-- capability schema fixture\n", encoding="utf-8")
    sbom_sha = sha256_file(sbom_path)
    receipt = {
        "schema_version": "ai-platform/agent-runtime-source-receipt/v1",
        "source": {
            "upstream_url": "https://github.com/openai/codex.git",
            "upstream_sha": "a" * 40,
            "fork_sha": "b" * 40,
            "git_tree_sha": "c" * 40,
        },
        "toolchain": {
            "cargo": "cargo 1.95.0",
            "rustc": "rustc 1.95.0",
            "rust_toolchain_sha256": "2" * 64,
            "cargo_lock_sha256": "3" * 64,
        },
        "schema_bundle": {"sha256": "d" * 64, "file_count": 2, "total_bytes": 10},
        "license": {
            "spdx": "Apache-2.0",
            "license_sha256": "e" * 64,
            "upstream_notice_sha256": "f" * 64,
        },
        "sbom": {"sha256": sbom_sha, "component_count": 1},
        "capability_worker_schema_sha256": sha256_file(capability_schema_path),
        "capability_worker_sbom": {
            "format": "CycloneDX-1.5",
            "path": "deploy/agent-runtime-source/capability-worker-sbom.cdx.json",
            "sha256": sha256_file(capability_sbom_path),
        },
    }
    _write_json(receipt_path, receipt)
    digest = "sha256:" + "1" * 64
    any_runnable = app_server_runnable or agent_runtime_runnable or capability_worker_runnable
    lock = {
        "schema_version": "ai-platform/agent-runtime-source-lock/v2",
        "release_state": "local_image_locked" if any_runnable else "local_source_locked",
        "source": {
            "upstream_url": "https://github.com/openai/codex.git",
            "upstream_sha": "a" * 40,
            "fork_sha": "b" * 40,
            "git_tree_sha": "c" * 40,
            "fork_repository": "local://ai-platform-agent-runtime-source",
        },
        "build": {
            "cargo": "cargo 1.95.0",
            "rustc": "rustc 1.95.0",
            "rust_toolchain_sha256": "2" * 64,
            "cargo_lock_sha256": "3" * 64,
            "source_receipt": "deploy/agent-runtime-source/source-receipt.json",
            "source_receipt_sha256": sha256_file(receipt_path),
            "sbom": "deploy/agent-runtime-source/sbom.cdx.json",
            "sbom_sha256": sbom_sha,
            "app_server_schema_sha256": "d" * 64,
            "app_server_schema_file_count": 2,
            "capability_worker_schema_sha256": sha256_file(capability_schema_path),
            "capability_worker_sbom": "deploy/agent-runtime-source/capability-worker-sbom.cdx.json",
            "capability_worker_sbom_sha256": sha256_file(capability_sbom_path),
            "overlay_file_count": 0,
            "overlay_cargo_lock_sha256": "0" * 64,
        },
        "license": {
            "spdx": "Apache-2.0",
            "notice": "deploy/agent-runtime-source/NOTICE.md",
            "notice_sha256": sha256_file(notice_path),
            "upstream_license_sha256": "e" * 64,
            "upstream_notice_sha256": "f" * 64,
        },
        "oci": {
            "artifacts": {
                "app_server": {
                    "binary": "codex-app-server",
                    "protocol": "codex-app-server-jsonrpc/v2",
                    "candidate_start_allowed": app_server_runnable,
                    "image_digest": digest if app_server_runnable else None,
                    "image_ref": f"local-image://{digest}" if app_server_runnable else None,
                    "platforms": ["linux/arm64"],
                },
                "agent_runtime": {
                    "binary": "ai-platform-agent-runtime",
                    "protocol": "ai-platform-agent-runtime-http-sse/v1",
                    "candidate_start_allowed": agent_runtime_runnable,
                    "image_digest": digest if agent_runtime_runnable else None,
                    "image_ref": f"local-image://{digest}" if agent_runtime_runnable else None,
                    "platforms": ["linux/arm64"],
                },
                "capability_worker": {
                    "binary": "ai-platform-capability-worker",
                    "protocol": "ai-platform-capability-contract/v2",
                    "candidate_start_allowed": capability_worker_runnable,
                    "image_digest": digest if capability_worker_runnable else None,
                    "image_ref": (
                        f"local-image://{digest}" if capability_worker_runnable else None
                    ),
                    "platforms": ["linux/arm64"],
                },
            }
        },
    }
    lock_path = tmp_path / "deploy/agent-runtime-source/lock.json"
    _write_json(lock_path, lock)
    return lock_path


def test_valid_digest_pinned_lock_passes(tmp_path: Path) -> None:
    validate_lock(
        repo_root=tmp_path,
        lock_path=_fixture(tmp_path),
        required_artifact="agent_runtime",
    )


def test_source_only_lock_fails_runnable_gate(tmp_path: Path) -> None:
    lock_path = _fixture(
        tmp_path,
        app_server_runnable=True,
        agent_runtime_runnable=False,
    )
    with pytest.raises(ContractError, match="agent_runtime.*not locked"):
        validate_lock(
            repo_root=tmp_path,
            lock_path=lock_path,
            required_artifact="agent_runtime",
        )


@pytest.mark.parametrize("target", ["receipt", "sbom", "notice"])
def test_tampered_artifact_fails_closed(tmp_path: Path, target: str) -> None:
    lock_path = _fixture(tmp_path)
    paths = {
        "receipt": tmp_path / "deploy/agent-runtime-source/source-receipt.json",
        "sbom": tmp_path / "deploy/agent-runtime-source/sbom.cdx.json",
        "notice": tmp_path / "deploy/agent-runtime-source/NOTICE.md",
    }
    paths[target].write_text("tampered\n", encoding="utf-8")
    with pytest.raises(ContractError):
        validate_lock(
            repo_root=tmp_path,
            lock_path=lock_path,
            required_artifact="app_server",
        )


def test_mismatched_local_image_reference_fails_closed(tmp_path: Path) -> None:
    lock_path = _fixture(tmp_path)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock["oci"]["artifacts"]["agent_runtime"]["image_ref"] = "registry.example/runtime:latest"
    _write_json(lock_path, lock)
    with pytest.raises(ContractError, match="local-image"):
        validate_lock(
            repo_root=tmp_path,
            lock_path=lock_path,
            required_artifact="agent_runtime",
        )


def test_artifact_binary_identity_is_mandatory(tmp_path: Path) -> None:
    lock_path = _fixture(tmp_path)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock["oci"]["artifacts"]["agent_runtime"]["binary"] = "codex-app-server"
    _write_json(lock_path, lock)
    with pytest.raises(ContractError, match="ai-platform-agent-runtime"):
        validate_lock(repo_root=tmp_path, lock_path=lock_path)


def test_source_refresh_invalidates_every_image(tmp_path: Path) -> None:
    lock_path = _fixture(tmp_path)

    refresh_source_lock(repo_root=tmp_path, lock_path=lock_path)

    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    assert lock["release_state"] == "local_source_locked"
    for artifact in lock["oci"]["artifacts"].values():
        assert artifact["candidate_start_allowed"] is False
        assert artifact["image_digest"] is None
        assert artifact["image_ref"] is None


@pytest.mark.parametrize(
    "manifest_schema",
    ["ai-platform/agent-runtime-overlay/v1", "ai-platform/agent-runtime-overlay/v2"],
)
def test_refresh_overlay_rebuilds_manifest_receipt_lock_and_dependency_sbom(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    manifest_schema: str,
) -> None:
    lock_path = _fixture(tmp_path)
    overlay_root = tmp_path / "rust/agent-runtime-overlay"
    kernel_root = overlay_root / "kernel-rs"
    kernel_root.mkdir(parents=True)
    (kernel_root / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
    (kernel_root / "Cargo.lock").write_text("overlay lock\n", encoding="utf-8")
    for package in (
        "ai-platform-capability-contract",
        "ai-platform-capability-worker",
    ):
        package_root = kernel_root / package
        package_root.mkdir()
        (package_root / "Cargo.toml").write_text(
            f'[package]\nname = "{package}"\nversion = "0.0.0"\n',
            encoding="utf-8",
        )
    (kernel_root / "src.rs").write_text("overlay\n", encoding="utf-8")
    _write_json(
        overlay_root / "manifest.json",
        {
            "schema_version": manifest_schema,
            "upstream_sha": "0" * 40,
            "source_revision": "0" * 40,
            "file_count": 0,
            "sha256": "0" * 64,
        },
    )
    receipt_path = tmp_path / "deploy/agent-runtime-source/source-receipt.json"
    receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    receipt["overlay"] = {
        "sha256": "0" * 64,
        "file_count": 0,
        "cargo_lock_sha256": "0" * 64,
        "source_revision": "b" * 40,
        "upstream_sha": "a" * 40,
    }
    _write_json(receipt_path, receipt)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock["build"].update(
        {
            "overlay_manifest": "rust/agent-runtime-overlay/manifest.json",
            "overlay_sha256": "0" * 64,
            "overlay_file_count": 0,
            "overlay_cargo_lock_sha256": "0" * 64,
            "source_receipt_sha256": sha256_file(receipt_path),
        }
    )
    _write_json(lock_path, lock)

    cargo_workspace = tmp_path / "controlled/codex-rs"
    cargo_workspace.mkdir(parents=True)
    for relative in (
        "Cargo.toml",
        "Cargo.lock",
        "ai-platform-capability-contract/Cargo.toml",
        "ai-platform-capability-worker/Cargo.toml",
    ):
        destination = cargo_workspace / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes((kernel_root / relative).read_bytes())
    worker_id = "path+file:///controlled/codex-rs#ai-platform-capability-worker@0.0.0"
    contract_id = "path+file:///controlled/codex-rs#ai-platform-capability-contract@0.0.0"
    serde_id = "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"
    metadata = {
        "packages": [
            {"id": worker_id, "name": "ai-platform-capability-worker", "version": "0.0.0"},
            {"id": contract_id, "name": "ai-platform-capability-contract", "version": "0.0.0"},
            {"id": serde_id, "name": "serde", "version": "1.0.0", "license": "MIT"},
        ],
        "resolve": {
            "nodes": [
                {"id": worker_id, "dependencies": [{"pkg": contract_id}]},
                {"id": contract_id, "dependencies": [{"pkg": serde_id}]},
                {"id": serde_id, "dependencies": []},
            ]
        },
    }
    monkeypatch.setattr(
        supply_chain,
        "_run",
        lambda *_args, **_kwargs: json.dumps(metadata),
    )

    supply_chain.refresh_overlay(
        repo_root=tmp_path,
        lock_path=lock_path,
        cargo_workspace=cargo_workspace,
    )

    actual = supply_chain.overlay_identity(overlay_root)
    manifest = json.loads((overlay_root / "manifest.json").read_text(encoding="utf-8"))
    receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    sbom_path = tmp_path / "deploy/agent-runtime-source/capability-worker-sbom.cdx.json"
    sbom = json.loads(sbom_path.read_text(encoding="utf-8"))
    overlay_lock_sha = sha256_file(kernel_root / "Cargo.lock")
    assert manifest["sha256"] == actual["sha256"]
    assert manifest["file_count"] == actual["file_count"]
    assert manifest["cargo_lock_sha256"] == overlay_lock_sha
    assert receipt["capability_worker_schema_sha256"] == sha256_file(
        tmp_path / "database/migrations/096_agent_capability_executions.sql"
    )
    assert {component["name"] for component in sbom["components"]} == {
        "ai-platform-capability-worker",
        "ai-platform-capability-contract",
        "serde",
    }
    assert len(sbom["dependencies"]) == 3
    assert lock["release_state"] == "local_source_locked"
    if manifest_schema == "ai-platform/agent-runtime-overlay/v2":
        original_manifest = (overlay_root / "manifest.json").read_bytes()
        assert lock["build"]["overlay_manifest_sha256"] == sha256_file(
            overlay_root / "manifest.json"
        )
        manifest["schema_version"] = "ai-platform/agent-runtime-overlay/v1"
        _write_json(overlay_root / "manifest.json", manifest)
        with pytest.raises(ContractError, match="upstream-base manifest is not pinned"):
            validate_lock(repo_root=tmp_path, lock_path=lock_path)
        (overlay_root / "manifest.json").write_bytes(original_manifest)
    assert all(
        not artifact["candidate_start_allowed"]
        and artifact["image_digest"] is None
        and artifact["image_ref"] is None
        for artifact in lock["oci"]["artifacts"].values()
    )
    validate_lock(repo_root=tmp_path, lock_path=lock_path)

    supply_chain.refresh_source_lock(repo_root=tmp_path, lock_path=lock_path)
    refreshed = json.loads(lock_path.read_text(encoding="utf-8"))
    assert refreshed["build"]["overlay_file_count"] == actual["file_count"]
    assert refreshed["build"]["overlay_cargo_lock_sha256"] == overlay_lock_sha

    (cargo_workspace / "Cargo.lock").write_text("stale composed lock\n", encoding="utf-8")
    with pytest.raises(ContractError, match="current overlay Cargo.lock"):
        supply_chain.refresh_overlay(
            repo_root=tmp_path,
            lock_path=lock_path,
            cargo_workspace=cargo_workspace,
        )


def test_record_local_image_checks_labels_and_locks_only_target(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    lock_path = _fixture(
        tmp_path,
        app_server_runnable=False,
        agent_runtime_runnable=False,
    )
    image_digest = "sha256:" + "9" * 64
    inspected = [
        {
            "Id": image_digest,
            "Os": "linux",
            "Architecture": "arm64",
            "Config": {
                "Labels": {
                    "org.opencontainers.image.revision": "b" * 40,
                    "com.misaya.ai-platform.agent-runtime.schema-sha256": "d" * 64,
                    "com.misaya.ai-platform.agent-runtime.artifact": "agent_runtime",
                    "com.misaya.ai-platform.agent-runtime.binary": "ai-platform-agent-runtime",
                }
            },
        }
    ]
    monkeypatch.setattr(supply_chain, "_run", lambda *_args, **_kwargs: json.dumps(inspected))

    record_local_image(
        repo_root=tmp_path,
        lock_path=lock_path,
        artifact_id="agent_runtime",
        image="runtime:test",
    )

    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    runtime = lock["oci"]["artifacts"]["agent_runtime"]
    assert runtime["image_digest"] == image_digest
    assert runtime["image_ref"] == f"local-image://{image_digest}"
    assert runtime["platforms"] == ["linux/arm64"]
    assert lock["oci"]["artifacts"]["app_server"]["candidate_start_allowed"] is False


def _locked_image_info(lock: dict, artifact_id: str) -> dict:
    worker = artifact_id == "capability_worker"
    prefix = (
        "com.misaya.ai-platform.capability-worker"
        if worker
        else "com.misaya.ai-platform.agent-runtime"
    )
    return {
        "Id": lock["oci"]["artifacts"][artifact_id]["image_digest"],
        "Os": "linux",
        "Architecture": "arm64",
        "Config": {
            "Labels": {
                "org.opencontainers.image.revision": lock["source"]["fork_sha"],
                f"{prefix}.schema-sha256": lock["build"][
                    "capability_worker_schema_sha256" if worker else "app_server_schema_sha256"
                ],
                f"{prefix}.artifact": artifact_id,
                f"{prefix}.binary": supply_chain.ARTIFACT_BINARIES[artifact_id],
            }
        },
    }


@pytest.mark.parametrize(
    "mismatch", [None, "worker_digest", "worker_source", "worker_platform", "runtime_digest"]
)
def test_selected_runtime_worker_pair_must_match_actual_locked_images(
    tmp_path, monkeypatch, mismatch
):
    lock_path = _fixture(tmp_path, capability_worker_runnable=True)
    lock = json.loads(lock_path.read_text())
    images = {
        "runtime:chosen": _locked_image_info(lock, "agent_runtime"),
        "worker:chosen": _locked_image_info(lock, "capability_worker"),
    }
    if mismatch == "worker_digest":
        images["worker:chosen"]["Id"] = "sha256:" + "0" * 64
    elif mismatch == "runtime_digest":
        images["runtime:chosen"]["Id"] = "sha256:" + "0" * 64
    elif mismatch == "worker_source":
        images["worker:chosen"]["Config"]["Labels"]["org.opencontainers.image.revision"] = "0" * 40
    elif mismatch == "worker_platform":
        images["worker:chosen"]["Architecture"] = "amd64"
    monkeypatch.setattr(
        supply_chain, "_run", lambda command, **_kwargs: json.dumps([images[command[-1]]])
    )
    before = lock_path.read_bytes()
    if mismatch:
        with pytest.raises(ContractError):
            supply_chain.verify_local_images(
                repo_root=tmp_path,
                lock_path=lock_path,
                runtime_image="runtime:chosen",
                worker_image="worker:chosen",
            )
    else:
        supply_chain.verify_local_images(
            repo_root=tmp_path,
            lock_path=lock_path,
            runtime_image="runtime:chosen",
            worker_image="worker:chosen",
        )
    assert lock_path.read_bytes() == before


def test_missing_worker_lock_fails_before_docker_inspection(tmp_path, monkeypatch):
    lock_path = _fixture(tmp_path, capability_worker_runnable=False)

    def unexpected(*_args, **_kwargs):
        pytest.fail("must reject the incomplete release unit before inspecting Docker")

    monkeypatch.setattr(supply_chain, "_run", unexpected)
    with pytest.raises(ContractError, match="capability_worker.*not locked"):
        supply_chain.verify_local_images(repo_root=tmp_path, lock_path=lock_path)


def test_worker_full_overlay_label_is_checked_before_locking_image(tmp_path):
    lock_path = _fixture(tmp_path, capability_worker_runnable=True)
    lock = json.loads(lock_path.read_text())
    lock["build"]["overlay_sha256"] = "4" * 64
    info = _locked_image_info(lock, "capability_worker")
    labels = info["Config"]["Labels"]
    labels["org.opencontainers.image.revision"] = lock["source"]["upstream_sha"] + "+" + "4" * 12
    labels["com.misaya.ai-platform.capability-worker.overlay-sha256"] = "4" * 12 + "5" * 52
    with pytest.raises(ContractError, match="labels"):
        supply_chain._checked_image_identity(lock, "capability_worker", info)
