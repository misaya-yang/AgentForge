#!/usr/bin/env python3
"""Generate and validate the pinned Agent Harness supply-chain receipt."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any

LOCK_SCHEMA = "ai-platform/agent-runtime-source-lock/v2"
RECEIPT_SCHEMA = "ai-platform/agent-runtime-source-receipt/v1"
ARTIFACT_BINARIES = {
    "app_server": "codex-app-server",
    "agent_runtime": "ai-platform-agent-runtime",
    "capability_worker": "ai-platform-capability-worker",
}
_HEX_40 = re.compile(r"^[0-9a-f]{40}$")
_HEX_64 = re.compile(r"^[0-9a-f]{64}$")
OVERLAY_MANIFEST_REL = "rust/agent-runtime-overlay/manifest.json"
CAPABILITY_WORKER_SCHEMA_REL = "database/migrations/096_agent_capability_executions.sql"
CAPABILITY_WORKER_SBOM_REL = "deploy/agent-runtime-source/capability-worker-sbom.cdx.json"


class ContractError(ValueError):
    """Raised when an immutable Harness identity is missing or inconsistent."""


def _run(command: list[str], *, cwd: Path) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def bundle_identity(root: Path) -> dict[str, Any]:
    files = sorted(path for path in root.rglob("*") if path.is_file())
    if not files:
        raise ContractError(f"schema bundle is empty: {root}")
    digest = hashlib.sha256()
    total_bytes = 0
    for path in files:
        relative = path.relative_to(root).as_posix().encode("utf-8")
        payload = path.read_bytes()
        total_bytes += len(payload)
        digest.update(relative)
        digest.update(b"\0")
        digest.update(payload)
        digest.update(b"\0")
    return {
        "sha256": digest.hexdigest(),
        "file_count": len(files),
        "total_bytes": total_bytes,
    }


def overlay_identity(root: Path) -> dict[str, Any]:
    """Hash the platform overlay without including its self-describing manifest."""
    files = sorted(
        path for path in root.rglob("*") if path.is_file() and path.name != "manifest.json"
    )
    if not files:
        raise ContractError(f"Agent Runtime overlay is empty: {root}")
    digest = hashlib.sha256()
    for path in files:
        relative = path.relative_to(root).as_posix().encode("utf-8")
        payload = path.read_bytes()
        digest.update(relative)
        digest.update(b"\0")
        digest.update(payload)
        digest.update(b"\0")
    return {"sha256": digest.hexdigest(), "file_count": len(files)}


def _cargo_components(metadata: dict[str, Any]) -> list[dict[str, Any]]:
    components: list[dict[str, Any]] = []
    for package in metadata.get("packages", []):
        name = str(package["name"])
        version = str(package["version"])
        component: dict[str, Any] = {
            "type": "library",
            "bom-ref": f"pkg:cargo/{name}@{version}",
            "name": name,
            "version": version,
            "purl": f"pkg:cargo/{name}@{version}",
        }
        license_value = package.get("license")
        if isinstance(license_value, str) and license_value:
            component["licenses"] = [{"expression": license_value}]
        source = package.get("source")
        if isinstance(source, str) and source:
            component["properties"] = [{"name": "cargo:source", "value": source}]
        components.append(component)
    return sorted(components, key=lambda item: (item["name"], item["version"], item["bom-ref"]))


def _cargo_dependency_closure(
    metadata: dict[str, Any], *, root_name: str
) -> tuple[str, set[str], dict[str, str]]:
    """Return the resolved package IDs and their deterministic SBOM references."""
    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        raise ContractError("cargo metadata must contain packages and resolve")
    by_id = {
        package.get("id"): package
        for package in packages
        if isinstance(package, dict) and isinstance(package.get("id"), str)
    }
    roots = [
        package["id"]
        for package in packages
        if isinstance(package, dict) and package.get("name") == root_name
    ]
    if len(roots) != 1:
        raise ContractError(f"cargo metadata must contain exactly one {root_name} package")
    nodes = {
        node.get("id"): node
        for node in resolve.get("nodes", [])
        if isinstance(node, dict) and isinstance(node.get("id"), str)
    }
    if roots[0] not in nodes:
        raise ContractError(f"cargo metadata resolve graph does not contain {root_name}")

    closure: set[str] = set()
    pending = [roots[0]]
    while pending:
        package_id = pending.pop()
        if package_id in closure:
            continue
        if package_id not in by_id or package_id not in nodes:
            raise ContractError(
                f"cargo metadata resolve graph references unknown package {package_id}"
            )
        closure.add(package_id)
        dependencies = nodes[package_id].get("dependencies", [])
        if not isinstance(dependencies, list):
            raise ContractError(f"cargo metadata dependencies for {package_id} must be an array")
        for dependency in dependencies:
            dependency_id = dependency.get("pkg") if isinstance(dependency, dict) else dependency
            if not isinstance(dependency_id, str):
                raise ContractError(f"cargo metadata has an invalid dependency for {package_id}")
            pending.append(dependency_id)

    refs: dict[str, str] = {}
    for package_id in sorted(closure):
        package = by_id[package_id]
        name = str(package["name"])
        version = str(package["version"])
        source = package.get("source")
        # Cargo can resolve the same name/version from different sources. Keep
        # refs unique without depending on package ordering or host paths.
        suffix = ""
        if isinstance(source, str) and source:
            suffix = "?source=" + hashlib.sha256(source.encode("utf-8")).hexdigest()[:16]
        refs[package_id] = f"pkg:cargo/{name}@{version}{suffix}"
    if len(set(refs.values())) != len(refs):
        raise ContractError("cargo metadata produced duplicate deterministic SBOM references")
    return roots[0], closure, refs


def _capability_worker_sbom(
    metadata: dict[str, Any],
    *,
    overlay: dict[str, Any],
    schema_sha: str,
    cargo_lock_sha: str,
    source: dict[str, Any],
) -> dict[str, Any]:
    root_id, closure, refs = _cargo_dependency_closure(
        metadata, root_name=ARTIFACT_BINARIES["capability_worker"]
    )
    packages = {
        package["id"]: package
        for package in metadata["packages"]
        if isinstance(package, dict) and isinstance(package.get("id"), str)
    }
    components = []
    for package_id in sorted(closure, key=lambda item: refs[item]):
        package = packages[package_id]
        component: dict[str, Any] = {
            "type": "library"
            if package["name"] != ARTIFACT_BINARIES["capability_worker"]
            else "application",
            "bom-ref": refs[package_id],
            "name": str(package["name"]),
            "version": str(package["version"]),
            "purl": refs[package_id],
        }
        license_value = package.get("license")
        if isinstance(license_value, str) and license_value:
            component["licenses"] = [{"expression": license_value}]
        components.append(component)

    nodes = {
        node.get("id"): node
        for node in metadata["resolve"].get("nodes", [])
        if isinstance(node, dict) and isinstance(node.get("id"), str)
    }
    dependencies = []
    for package_id in sorted(closure, key=lambda item: refs[item]):
        depends_on = []
        for dependency in nodes[package_id].get("dependencies", []):
            dependency_id = dependency.get("pkg") if isinstance(dependency, dict) else dependency
            if dependency_id in closure:
                depends_on.append(refs[dependency_id])
        dependencies.append({"ref": refs[package_id], "dependsOn": sorted(set(depends_on))})

    serial = uuid.uuid5(
        uuid.NAMESPACE_URL,
        "ai-platform-capability-worker:"
        f"{source.get('fork_sha')}:{overlay['sha256']}:{schema_sha}:{cargo_lock_sha}",
    )
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": refs[root_id],
                "name": ARTIFACT_BINARIES["capability_worker"],
                "version": f"{source.get('upstream_sha')}+{overlay['sha256'][:12]}",
                "properties": [
                    {"name": "ai-platform:overlay.sha256", "value": overlay["sha256"]},
                    {"name": "ai-platform:capability-schema.sha256", "value": schema_sha},
                    {"name": "ai-platform:cargo-lock.sha256", "value": cargo_lock_sha},
                ],
            }
        },
        "components": components,
        "dependencies": dependencies,
    }


def generate_receipt(*, fork: Path, schema_dir: Path, receipt_path: Path, sbom_path: Path) -> None:
    fork = fork.resolve()
    schema_dir = schema_dir.resolve()
    if _run(["git", "status", "--porcelain"], cwd=fork):
        raise ContractError("Agent Harness fork must be clean before generating a source receipt")

    fork_sha = _run(["git", "rev-parse", "HEAD"], cwd=fork)
    upstream_sha = _run(["git", "merge-base", "HEAD", "upstream/main"], cwd=fork)
    tree_sha = _run(["git", "rev-parse", "HEAD^{tree}"], cwd=fork)
    branch = _run(["git", "branch", "--show-current"], cwd=fork)
    upstream_url = _run(["git", "remote", "get-url", "upstream"], cwd=fork)
    cargo_dir = fork / "codex-rs"
    metadata = json.loads(
        _run(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=cargo_dir)
    )
    cargo_version = _run(["cargo", "--version"], cwd=cargo_dir)
    rustc_version = _run(["rustc", "--version"], cwd=cargo_dir)

    components = _cargo_components(metadata)
    # The controlled fork revision is the runnable product artifact.  Seeding the
    # SBOM from the upstream merge-base would make distinct fork releases share
    # an identity even when their source and dependency graph differ.
    serial = uuid.uuid5(
        uuid.NAMESPACE_URL,
        f"ai-platform-agent-runtime:{fork_sha}:{tree_sha}",
    )
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": f"urn:git:{fork_sha}",
                "name": "ai-platform-agent-runtime-source",
                "version": fork_sha,
                "properties": [
                    {"name": "ai-platform:upstream.url", "value": upstream_url},
                    {"name": "ai-platform:upstream.sha", "value": upstream_sha},
                    {"name": "ai-platform:git.tree", "value": tree_sha},
                ],
            }
        },
        "components": components,
    }
    sbom_path.parent.mkdir(parents=True, exist_ok=True)
    sbom_path.write_text(
        json.dumps(sbom, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    schema = bundle_identity(schema_dir)
    receipt = {
        "schema_version": RECEIPT_SCHEMA,
        "source": {
            "upstream_url": upstream_url,
            "upstream_sha": upstream_sha,
            "fork_sha": fork_sha,
            "git_tree_sha": tree_sha,
            "branch": branch,
        },
        "toolchain": {
            "cargo": cargo_version,
            "rustc": rustc_version,
            "rust_toolchain_sha256": sha256_file(cargo_dir / "rust-toolchain.toml"),
            "cargo_lock_sha256": sha256_file(cargo_dir / "Cargo.lock"),
        },
        "schema_bundle": schema,
        "license": {
            "spdx": "Apache-2.0",
            "license_sha256": sha256_file(fork / "LICENSE"),
            "upstream_notice_sha256": sha256_file(fork / "NOTICE"),
        },
        "sbom": {
            "format": "CycloneDX-1.5",
            "component_count": len(components),
            "sha256": sha256_file(sbom_path),
        },
    }
    receipt_path.parent.mkdir(parents=True, exist_ok=True)
    receipt_path.write_text(
        json.dumps(receipt, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def _load_object(path: Path, *, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ContractError(f"{label} is unreadable: {path}") from exc
    if not isinstance(value, dict):
        raise ContractError(f"{label} must be a JSON object")
    return value


def _write_object_atomic(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=f".{path.name}.",
        delete=False,
    ) as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2, sort_keys=True)
        handle.write("\n")
        temporary = Path(handle.name)
    temporary.replace(path)


def _require_hex(value: Any, *, length: int, label: str) -> str:
    pattern = _HEX_40 if length == 40 else _HEX_64
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise ContractError(f"{label} must be {length} lowercase hexadecimal characters")
    return value


def _validate_oci_artifact(
    *,
    artifact_id: str,
    artifact: dict[str, Any],
    release_state: str,
) -> bool:
    expected_binary = ARTIFACT_BINARIES[artifact_id]
    if artifact.get("binary") != expected_binary:
        raise ContractError(f"oci.artifacts.{artifact_id}.binary must be {expected_binary}")
    if not isinstance(artifact.get("protocol"), str) or not artifact["protocol"]:
        raise ContractError(f"oci.artifacts.{artifact_id}.protocol is required")
    platforms = artifact.get("platforms")
    if not isinstance(platforms, list) or not all(
        isinstance(platform, str) and platform for platform in platforms
    ):
        raise ContractError(f"oci.artifacts.{artifact_id}.platforms must be a string array")

    candidate_start_allowed = artifact.get("candidate_start_allowed") is True
    digest = artifact.get("image_digest")
    image_ref = artifact.get("image_ref")
    if not candidate_start_allowed:
        if digest is not None or image_ref is not None:
            raise ContractError(
                f"unlocked oci artifact {artifact_id} must not carry an image identity"
            )
        return False

    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ContractError(f"oci.artifacts.{artifact_id}.image_digest must be immutable")
    if release_state == "local_image_locked":
        if image_ref != f"local-image://{digest}":
            raise ContractError(
                f"local oci.artifacts.{artifact_id}.image_ref must equal local-image://<digest>"
            )
    elif release_state == "published":
        if not isinstance(image_ref, str) or not image_ref.endswith(f"@{digest}"):
            raise ContractError(
                f"published oci.artifacts.{artifact_id}.image_ref must be digest-pinned"
            )
    else:
        raise ContractError("a runnable image cannot use local_source_locked release state")
    return True


def validate_lock(
    *,
    repo_root: Path,
    lock_path: Path,
    require_runnable: bool = False,
    required_artifact: str | None = None,
) -> None:
    lock = _load_object(lock_path, label="Agent Harness lock")
    if lock.get("schema_version") != LOCK_SCHEMA:
        raise ContractError(f"unsupported lock schema: {lock.get('schema_version')!r}")

    release_state = lock.get("release_state")
    if release_state not in {"local_source_locked", "local_image_locked", "published"}:
        raise ContractError(
            "release_state must be local_source_locked, local_image_locked, or published"
        )

    source = lock.get("source")
    build = lock.get("build")
    license_info = lock.get("license")
    oci = lock.get("oci")
    if not all(isinstance(value, dict) for value in (source, build, license_info, oci)):
        raise ContractError("source, build, license, and oci sections must be objects")

    upstream_sha = _require_hex(source.get("upstream_sha"), length=40, label="source.upstream_sha")
    fork_sha = _require_hex(source.get("fork_sha"), length=40, label="source.fork_sha")
    if source.get("upstream_url") != "https://github.com/openai/codex.git":
        raise ContractError("source.upstream_url must identify the canonical HTTPS upstream")
    if not isinstance(source.get("fork_repository"), str) or not source["fork_repository"]:
        raise ContractError("source.fork_repository is required")

    receipt_rel = build.get("source_receipt")
    sbom_rel = build.get("sbom")
    notice_rel = license_info.get("notice")
    if not all(isinstance(value, str) and value for value in (receipt_rel, sbom_rel, notice_rel)):
        raise ContractError("source receipt, SBOM, and notice paths are required")
    receipt_path = repo_root / receipt_rel
    sbom_path = repo_root / sbom_rel
    notice_path = repo_root / notice_rel
    receipt = _load_object(receipt_path, label="Agent Harness source receipt")

    if receipt.get("schema_version") != RECEIPT_SCHEMA:
        raise ContractError("source receipt schema does not match the supported contract")
    overlay = receipt.get("overlay") or {}
    actual_overlay = None
    if build.get("overlay_manifest"):
        overlay_root = repo_root / "rust/agent-runtime-overlay"
        actual_overlay = overlay_identity(overlay_root)
        manifest = _load_object(
            overlay_root / "manifest.json", label="Agent Runtime overlay manifest"
        )
        manifest_schema = manifest.get("schema_version")
        if manifest_schema == "ai-platform/agent-runtime-overlay/v2" or build.get(
            "overlay_manifest_schema"
        ) == "ai-platform/agent-runtime-overlay/v2":
            manifest_digest = sha256_file(overlay_root / "manifest.json")
            if (
                manifest_schema != "ai-platform/agent-runtime-overlay/v2"
                or build.get("overlay_manifest_schema") != manifest_schema
                or overlay.get("manifest_schema") != manifest_schema
                or build.get("overlay_manifest_sha256") != manifest_digest
                or overlay.get("manifest_sha256") != manifest_digest
            ):
                raise ContractError("overlay upstream-base manifest is not pinned by lock and receipt")
        if (
            manifest.get("sha256") != actual_overlay["sha256"]
            or manifest.get("file_count") != actual_overlay["file_count"]
        ):
            raise ContractError("Agent Runtime overlay manifest does not match its files")
        if manifest.get("source_revision") != source.get("fork_sha") or manifest.get(
            "upstream_sha"
        ) != source.get("upstream_sha"):
            raise ContractError("Agent Runtime overlay source identity does not match the lock")
        if (
            overlay.get("sha256") != actual_overlay["sha256"]
            or overlay.get("file_count") != actual_overlay["file_count"]
        ):
            raise ContractError("source receipt overlay identity does not match the overlay")
        overlay_lock_sha = sha256_file(
            repo_root / "rust/agent-runtime-overlay/kernel-rs/Cargo.lock"
        )
        if manifest.get("cargo_lock_sha256") != overlay_lock_sha:
            raise ContractError(
                "Agent Runtime overlay Cargo.lock digest does not match the manifest"
            )
        if overlay.get("cargo_lock_sha256") != overlay_lock_sha:
            raise ContractError(
                "source receipt overlay Cargo.lock digest does not match the overlay"
            )
    receipt_source = receipt.get("source") or {}
    if (
        receipt_source.get("upstream_sha") != upstream_sha
        or receipt_source.get("fork_sha") != fork_sha
    ):
        raise ContractError("lock source SHAs do not match the source receipt")
    receipt_tree_sha = _require_hex(
        receipt_source.get("git_tree_sha"), length=40, label="receipt git tree SHA"
    )
    if receipt_tree_sha != source.get("git_tree_sha"):
        raise ContractError("lock git tree SHA does not match the source receipt")
    if receipt_source.get("upstream_url") != source.get("upstream_url"):
        raise ContractError("lock upstream URL does not match the source receipt")

    expected_receipt_sha = _require_hex(
        build.get("source_receipt_sha256"), length=64, label="build.source_receipt_sha256"
    )
    if sha256_file(receipt_path) != expected_receipt_sha:
        raise ContractError("source receipt digest does not match the lock")
    expected_sbom_sha = _require_hex(build.get("sbom_sha256"), length=64, label="build.sbom_sha256")
    if not sbom_path.is_file() or sha256_file(sbom_path) != expected_sbom_sha:
        raise ContractError("SBOM is missing or its digest does not match the lock")
    receipt_sbom = receipt.get("sbom") or {}
    if receipt_sbom.get("sha256") != expected_sbom_sha:
        raise ContractError("source receipt SBOM digest does not match the lock")
    if build.get("overlay_manifest"):
        if build.get("overlay_manifest") != OVERLAY_MANIFEST_REL:
            raise ContractError(
                "build.overlay_manifest must point to the Agent Runtime overlay manifest"
            )
        if (
            build.get("overlay_sha256") != actual_overlay["sha256"]
            or build.get("overlay_file_count") != actual_overlay["file_count"]
        ):
            raise ContractError("build.overlay_sha256 does not match the Agent Runtime overlay")
        if build.get("overlay_cargo_lock_sha256") != overlay_lock_sha:
            raise ContractError("build overlay Cargo.lock digest does not match the overlay")
    capability_schema = build.get("capability_worker_schema_sha256")
    expected_capability_schema = sha256_file(repo_root / CAPABILITY_WORKER_SCHEMA_REL)
    if capability_schema != expected_capability_schema:
        raise ContractError("capability worker schema digest does not match migration 096")
    if receipt.get("capability_worker_schema_sha256") != expected_capability_schema:
        raise ContractError(
            "source receipt capability worker schema digest does not match migration 096"
        )
    capability_sbom_rel = build.get("capability_worker_sbom")
    capability_sbom_sha = build.get("capability_worker_sbom_sha256")
    receipt_capability_sbom = receipt.get("capability_worker_sbom") or {}
    if not isinstance(capability_sbom_rel, str) or not capability_sbom_rel:
        raise ContractError("capability worker SBOM path is required")
    capability_sbom_path = repo_root / capability_sbom_rel
    if (
        not capability_sbom_path.is_file()
        or sha256_file(capability_sbom_path) != capability_sbom_sha
        or capability_sbom_sha != receipt_capability_sbom.get("sha256")
        or capability_sbom_rel != receipt_capability_sbom.get("path")
    ):
        raise ContractError("capability worker SBOM does not match the source receipt")
    # The digest check above only proves the committed SBOM is the one the lock
    # names.  It cannot see that the SBOM still *describes* the locked overlay:
    # regenerating the lock without regenerating the SBOM leaves a document that
    # claims a different build than the image we ship.  Pin the identity too.
    capability_sbom = _load_object(capability_sbom_path, label="capability worker SBOM")
    sbom_component = (capability_sbom.get("metadata") or {}).get("component") or {}
    sbom_overlay_sha = next(
        (
            property_.get("value")
            for property_ in sbom_component.get("properties") or []
            if isinstance(property_, dict) and property_.get("name") == "ai-platform:overlay.sha256"
        ),
        None,
    )
    expected_overlay_sha = build.get("overlay_sha256")
    if expected_overlay_sha and (
        sbom_overlay_sha != expected_overlay_sha
        or sbom_component.get("version") != f"{source['upstream_sha']}+{expected_overlay_sha[:12]}"
    ):
        raise ContractError("capability worker SBOM does not describe the locked overlay")

    schema = receipt.get("schema_bundle") or {}
    _require_hex(schema.get("sha256"), length=64, label="schema bundle SHA")
    if not isinstance(schema.get("file_count"), int) or schema["file_count"] <= 0:
        raise ContractError("schema bundle file_count must be positive")
    if schema.get("sha256") != build.get("app_server_schema_sha256"):
        raise ContractError("App Server schema digest does not match the source receipt")
    if schema.get("file_count") != build.get("app_server_schema_file_count"):
        raise ContractError("App Server schema file count does not match the source receipt")

    toolchain = receipt.get("toolchain") or {}
    for field in ("cargo", "rustc", "rust_toolchain_sha256", "cargo_lock_sha256"):
        if toolchain.get(field) != build.get(field):
            raise ContractError(f"build.{field} does not match the source receipt")

    if license_info.get("spdx") != "Apache-2.0":
        raise ContractError("Agent Harness license must remain Apache-2.0")
    expected_notice_sha = _require_hex(
        license_info.get("notice_sha256"), length=64, label="license.notice_sha256"
    )
    if not notice_path.is_file() or sha256_file(notice_path) != expected_notice_sha:
        raise ContractError("Apache notice is missing or its digest does not match the lock")
    receipt_license = receipt.get("license") or {}
    if receipt_license.get("license_sha256") != license_info.get("upstream_license_sha256"):
        raise ContractError("upstream LICENSE digest does not match the source receipt")
    if receipt_license.get("upstream_notice_sha256") != license_info.get("upstream_notice_sha256"):
        raise ContractError("upstream NOTICE digest does not match the source receipt")

    artifacts = oci.get("artifacts")
    if not isinstance(artifacts, dict) or set(artifacts) != set(ARTIFACT_BINARIES):
        raise ContractError("oci.artifacts must define every locked runtime artifact")
    runnable = {
        artifact_id: _validate_oci_artifact(
            artifact_id=artifact_id,
            artifact=artifacts[artifact_id],
            release_state=release_state,
        )
        for artifact_id in ARTIFACT_BINARIES
        if isinstance(artifacts.get(artifact_id), dict)
    }
    if len(runnable) != len(ARTIFACT_BINARIES):
        raise ContractError("every oci artifact must be an object")
    if release_state == "local_source_locked" and any(runnable.values()):
        raise ContractError("local_source_locked cannot contain runnable OCI artifacts")
    if release_state in {"local_image_locked", "published"} and not any(runnable.values()):
        raise ContractError(f"{release_state} requires at least one runnable OCI artifact")

    required = required_artifact or ("app_server" if require_runnable else None)
    if required is not None:
        if required not in ARTIFACT_BINARIES:
            raise ContractError(f"unknown required OCI artifact: {required}")
        if not runnable[required]:
            raise ContractError(f"OCI artifact {required} is not locked to a runnable image")


def generate_composed_receipt(
    *, package: Path, upstream: Path, composed: Path, repo_root: Path,
    export_evidence: Path, schema_dir: Path, output: Path,
) -> None:
    """Generate source evidence offline; never mutate source or grant image admission.

    Unlike generate_receipt, metadata belongs to the verified composed workspace,
    while immutable Git provenance belongs to the clean upstream snapshot.
    """
    if __package__:
        from . import agent_runtime_source_candidate as candidate
    else:
        import agent_runtime_source_candidate as candidate
    import tomllib

    def require(condition: bool, message: str) -> None:
        if not condition:
            raise ContractError(message)

    package, upstream, composed, repo_root, schema_dir = (
        p.resolve() for p in (package, upstream, composed, repo_root, schema_dir)
    )
    export_evidence = export_evidence.resolve()
    output = output.resolve()
    require(not output.exists(), "output directory must not exist; source evidence is never overwritten")
    for root in (package, upstream, composed, repo_root, schema_dir, export_evidence.parent):
        require(not output.is_relative_to(root), "output must be external to all source/evidence inputs")
    unit = _load_object(package / "source-unit.json", label="source unit")
    try:
        candidate.verify_payload(package, unit)
        candidate.verify_upstream(package, unit, upstream, composed)
        candidate.verify_live_preimages(package, repo_root)
    except candidate.InvalidSource as error:
        raise ContractError(str(error)) from error
    require(not _run(["git", "status", "--porcelain", "--untracked-files=all"], cwd=upstream),
            "immutable upstream checkout must be clean")
    target = unit["target"]
    require(_run(["git", "rev-parse", "HEAD"], cwd=upstream) == target,
            "clean upstream HEAD must equal the source-unit target")
    require(_run(["git", "rev-parse", "HEAD^{tree}"], cwd=upstream) == unit["target_tree"],
            "clean upstream tree must equal the source-unit tree")
    require(_run(["git", "remote", "get-url", "origin"], cwd=upstream) == "https://github.com/openai/codex.git",
            "upstream origin must be the official immutable source repository")

    evidence = _load_object(export_evidence, label="composed export evidence")
    require(evidence.get("schema") == "ai-platform/composed-source-export/v1", "unsupported export evidence")
    unit_sha = sha256_file(package / "source-unit.json")
    require(evidence.get("source_unit_sha256_before") == unit_sha == evidence.get("source_unit_sha256_after"),
            "export must bind the same verified source unit before and after generation")
    require(evidence.get("target_commit") == target, "export target differs from source unit")
    lock_sha = sha256_file(composed / "codex-rs/Cargo.lock")
    require(evidence.get("resolved_lock_sha256") == lock_sha == unit["resolved_lock_sha256"],
            "export resolved lock differs from verified source")

    def exported(name: str) -> Path:
        record = evidence.get("files", {}).get(name)
        require(isinstance(record, dict), f"missing export file: {name}")
        try:
            path = candidate.safe_path(export_evidence.parent, record["path"])
        except (candidate.InvalidSource, KeyError) as error:
            raise ContractError(f"invalid export file: {name}") from error
        require(path.is_file() and sha256_file(path) == record.get("sha256"), f"export file changed: {name}")
        return path

    metadata_path = exported("cargo_metadata")
    require(sha256_file(exported("cargo_lock")) == lock_sha, "exported lock differs from composed lock")
    schema_run = _load_object(exported("schema_command_receipt"), label="schema command receipt")
    metadata_run = _load_object(exported("metadata_command_receipt"), label="metadata command receipt")
    for name, result in (("schema", schema_run), ("metadata", metadata_run)):
        require(result.get("exit_code") == 0 and result.get("stop_reason") is None,
                f"{name} export command did not succeed")
        require(isinstance(result.get("command"), list), f"{name} export command is missing")
        require(result.get("cwd") == str(composed / "codex-rs"),
                f"{name} export command belongs to another working directory")
    channel = tomllib.loads((composed / "codex-rs/rust-toolchain.toml").read_text())["toolchain"]["channel"]
    schema_command = schema_run["command"]
    require(schema_command[1:] == [f"+{channel}", "run", "--locked", "--offline", "-p",
            "codex-app-server-protocol", "--example", "ai_platform_export_schema", "--", str(schema_dir)],
            "schema must be generated by the exact compiled protocol example, offline and locked")
    require(metadata_run["command"][1:] == [f"+{channel}", "metadata", "--locked", "--offline",
            "--format-version", "1"], "metadata must be an unfiltered offline locked export")
    require(metadata_run.get("stdout_sha256") == sha256_file(metadata_path), "metadata command output is not hash-bound")
    require(schema_dir.is_dir() and not schema_dir.is_symlink(), "schema output must be a real directory")
    require(not any(p.is_symlink() for p in schema_dir.rglob("*")), "schema bundle cannot contain symlinks")
    schema = bundle_identity(schema_dir)
    require(evidence.get("schema_bundle") == schema, "compiled schema bundle differs from export evidence")
    for path in schema_dir.rglob("*.json"):
        json.loads(path.read_text())
    cargo_version = exported("cargo_version").read_text().strip()
    rustc_version = exported("rustc_version").read_text().strip()
    require(cargo_version.startswith(f"cargo {channel} ") and rustc_version.startswith(f"rustc {channel} "),
            "export toolchain versions do not match the source toolchain")

    metadata = _load_object(metadata_path, label="composed cargo metadata")
    require(Path(metadata.get("workspace_root", "")).resolve() == composed / "codex-rs",
            "Cargo metadata belongs to another workspace")
    packages = metadata.get("packages", [])
    members = set(metadata.get("workspace_members", []))
    names = {p["name"] for p in packages if p.get("id") in members}
    require({"ai-platform-agent-runtime", "ai-platform-capability-worker", "ai-platform-capability-contract",
             "ai-platform-office"} <= names, "composed metadata lacks the four platform workspace crates")
    lock = tomllib.loads((composed / "codex-rs/Cargo.lock").read_text())
    locked = {(p["name"], p["version"], p.get("source")) for p in lock.get("package", [])}
    for item in packages:
        require((item["name"], item["version"], item.get("source")) in locked,
                f"metadata package not present in exact resolved lock: {item['name']}")
        if item.get("source") is None:
            manifest = Path(item["manifest_path"]).resolve()
            require(manifest.is_relative_to(composed) and manifest.is_file(),
                    f"metadata path package is outside verified source: {item['name']}")

    source = {"upstream_url": "https://github.com/openai/codex.git", "upstream_sha": target,
              "fork_sha": target, "git_tree_sha": unit["target_tree"], "branch": ""}
    overlay = overlay_identity(package / "payload")
    manifest = {"schema_version": "ai-platform/agent-runtime-overlay/v2", "upstream_sha": target,
                "source_revision": target, **overlay, "cargo_lock_sha256": lock_sha,
                "upstream_blobs": {r["path"]: r["target_upstream_blob"] for r in unit["records"]}}
    components = _cargo_components(metadata)
    sbom = {"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
            "serialNumber": "urn:uuid:" + str(uuid.uuid5(uuid.NAMESPACE_URL,
                f"ai-platform-composed-source:{target}:{overlay['sha256']}:{lock_sha}")),
            "metadata": {"component": {"type": "application", "name": "ai-platform-agent-runtime-source",
                "bom-ref": f"urn:git:{target}:overlay:{overlay['sha256']}",
                "version": f"{target}+{overlay['sha256'][:12]}", "properties": [
                    {"name": "ai-platform:git.tree", "value": unit["target_tree"]},
                    {"name": "ai-platform:overlay.sha256", "value": overlay["sha256"]},
                    {"name": "ai-platform:cargo-lock.sha256", "value": lock_sha}]}},
            "components": components}
    schema_sha = sha256_file(repo_root / CAPABILITY_WORKER_SCHEMA_REL)
    worker_sbom = _capability_worker_sbom(metadata, overlay=overlay, schema_sha=schema_sha,
                                         cargo_lock_sha=lock_sha, source=source)

    # Validate all inputs before creating any output. These are newly generated
    # source artifacts; no existing source lock or startup flag is written.
    output.mkdir(parents=True)
    for name, value in (("manifest.json", manifest), ("sbom.cdx.json", sbom),
                        ("capability-worker-sbom.cdx.json", worker_sbom)):
        _write_object_atomic(output / name, value)
    receipt = {"schema_version": RECEIPT_SCHEMA, "source": source,
        "toolchain": {"cargo": cargo_version, "rustc": rustc_version,
            "rust_toolchain_sha256": sha256_file(composed / "codex-rs/rust-toolchain.toml"),
            "cargo_lock_sha256": lock_sha}, "schema_bundle": schema,
        "license": {"spdx": "Apache-2.0", "license_sha256": sha256_file(upstream / "LICENSE"),
                    "upstream_notice_sha256": sha256_file(upstream / "NOTICE")},
        "sbom": {"format": "CycloneDX-1.5", "component_count": len(components),
                 "sha256": sha256_file(output / "sbom.cdx.json")},
        "overlay": {**overlay, "upstream_sha": target, "source_revision": target,
                    "cargo_lock_sha256": lock_sha, "manifest_schema": manifest["schema_version"],
                    "manifest_sha256": sha256_file(output / "manifest.json")},
        "capability_worker_schema_sha256": schema_sha,
        "capability_worker_sbom": {"format": "CycloneDX-1.5", "path": CAPABILITY_WORKER_SBOM_REL,
            "sha256": sha256_file(output / "capability-worker-sbom.cdx.json"),
            "component_count": len(worker_sbom["components"])},
        "evidence_provenance": {"method": "verified immutable upstream plus source-only overlay; offline compiled protocol export",
            "source_unit_sha256": unit_sha, "export_evidence_sha256": sha256_file(export_evidence),
            "metadata_file_sha256": sha256_file(metadata_path),
            "schema_generated_by_compiled_protocol_library": True,
            "runtime_compiled": False, "worker_compiled": False, "docker_image_verified": False}}
    _write_object_atomic(output / "source-receipt.json", receipt)


def refresh_source_lock(*, repo_root: Path, lock_path: Path) -> None:
    lock = _load_object(lock_path, label="Agent Harness lock")
    if lock.get("schema_version") != LOCK_SCHEMA:
        raise ContractError("source lock must be upgraded before refresh")
    build = lock.get("build") or {}
    license_info = lock.get("license") or {}
    receipt_rel = build.get("source_receipt")
    sbom_rel = build.get("sbom")
    notice_rel = license_info.get("notice")
    if not all(isinstance(value, str) and value for value in (receipt_rel, sbom_rel, notice_rel)):
        raise ContractError("lock paths must be present before source refresh")
    receipt_path = repo_root / receipt_rel
    sbom_path = repo_root / sbom_rel
    notice_path = repo_root / notice_rel
    receipt = _load_object(receipt_path, label="Agent Harness source receipt")
    if receipt.get("schema_version") != RECEIPT_SCHEMA:
        raise ContractError("source receipt schema does not match the supported contract")
    source = receipt.get("source") or {}
    toolchain = receipt.get("toolchain") or {}
    schema = receipt.get("schema_bundle") or {}
    receipt_license = receipt.get("license") or {}
    receipt_sbom = receipt.get("sbom") or {}
    receipt_capability_sbom = receipt.get("capability_worker_sbom") or {}
    _require_hex(source.get("upstream_sha"), length=40, label="source upstream SHA")
    _require_hex(source.get("fork_sha"), length=40, label="source fork SHA")
    _require_hex(source.get("git_tree_sha"), length=40, label="source tree SHA")
    _require_hex(schema.get("sha256"), length=64, label="schema bundle SHA")
    if not sbom_path.is_file() or sha256_file(sbom_path) != receipt_sbom.get("sha256"):
        raise ContractError("generated SBOM does not match the source receipt")
    if not notice_path.is_file():
        raise ContractError("platform Apache notice is missing")

    fork_repository = (lock.get("source") or {}).get("fork_repository")
    if not isinstance(fork_repository, str) or not fork_repository:
        raise ContractError("source.fork_repository is required")
    lock["source"] = {
        "upstream_url": source.get("upstream_url"),
        "upstream_sha": source.get("upstream_sha"),
        "fork_repository": fork_repository,
        "fork_sha": source.get("fork_sha"),
        "git_tree_sha": source.get("git_tree_sha"),
        "branch": source.get("branch"),
    }
    lock["build"] = {
        "rustc": toolchain.get("rustc"),
        "cargo": toolchain.get("cargo"),
        "local_image_profile": build.get("local_image_profile", "dev-small"),
        "rust_toolchain_sha256": toolchain.get("rust_toolchain_sha256"),
        "cargo_lock_sha256": toolchain.get("cargo_lock_sha256"),
        "app_server_schema_sha256": schema.get("sha256"),
        "app_server_schema_file_count": schema.get("file_count"),
        "capability_worker_schema_sha256": sha256_file(
            repo_root / "database/migrations/096_agent_capability_executions.sql"
        ),
        "capability_worker_sbom": receipt_capability_sbom.get("path"),
        "capability_worker_sbom_sha256": receipt_capability_sbom.get("sha256"),
        "source_receipt": receipt_rel,
        "source_receipt_sha256": sha256_file(receipt_path),
        "sbom": sbom_rel,
        "sbom_sha256": sha256_file(sbom_path),
    }
    if build.get("overlay_manifest"):
        lock["build"]["overlay_manifest"] = build["overlay_manifest"]
        refreshed_overlay = receipt.get("overlay") or {}
        lock["build"]["overlay_sha256"] = refreshed_overlay.get("sha256")
        lock["build"]["overlay_file_count"] = refreshed_overlay.get("file_count")
        lock["build"]["overlay_cargo_lock_sha256"] = refreshed_overlay.get("cargo_lock_sha256")
        if (receipt.get("overlay") or {}).get("manifest_schema"):
            lock["build"]["overlay_manifest_schema"] = receipt["overlay"]["manifest_schema"]
            lock["build"]["overlay_manifest_sha256"] = receipt["overlay"].get("manifest_sha256")
    lock["license"] = {
        "spdx": "Apache-2.0",
        "notice": notice_rel,
        "notice_sha256": sha256_file(notice_path),
        "upstream_license_sha256": receipt_license.get("license_sha256"),
        "upstream_notice_sha256": receipt_license.get("upstream_notice_sha256"),
    }
    lock["release_state"] = "local_source_locked"
    artifacts = (lock.get("oci") or {}).get("artifacts") or {}
    for artifact_id, binary in ARTIFACT_BINARIES.items():
        artifact = artifacts.get(artifact_id) or {}
        artifacts[artifact_id] = {
            "binary": binary,
            "protocol": artifact.get("protocol"),
            "candidate_start_allowed": False,
            "image_ref": None,
            "image_digest": None,
            "platforms": artifact.get("platforms", ["linux/arm64"]),
        }
    lock["oci"] = {"artifacts": artifacts}
    _write_object_atomic(lock_path, lock)
    validate_lock(repo_root=repo_root, lock_path=lock_path)


def refresh_overlay(*, repo_root: Path, lock_path: Path, cargo_workspace: Path, cargo_metadata: Path | None = None) -> None:
    """Refresh overlay identities and the Worker dependency SBOM atomically.

    ``cargo_workspace`` is intentionally read-only: it is the already-composed,
    controlled runtime source used only for ``cargo metadata --locked``. The
    platform overlay and migration are hashed from the target repository, so a
    changed source unit cannot accidentally retain an old runnable image.
    """
    lock = _load_object(lock_path, label="Agent Harness lock")
    if lock.get("schema_version") != LOCK_SCHEMA:
        raise ContractError("source lock must be upgraded before overlay refresh")
    build = lock.get("build")
    if not isinstance(build, dict):
        raise ContractError("source lock build section is required")
    receipt_rel = build.get("source_receipt")
    if not isinstance(receipt_rel, str) or not receipt_rel:
        raise ContractError("source receipt path is required before overlay refresh")
    receipt_path = repo_root / receipt_rel
    receipt = _load_object(receipt_path, label="Agent Harness source receipt")
    if receipt.get("schema_version") != RECEIPT_SCHEMA:
        raise ContractError("source receipt schema does not match the supported contract")

    cargo_workspace = cargo_workspace.resolve()
    cargo_cwd = cargo_workspace.parent if cargo_workspace.is_file() else cargo_workspace
    cargo_manifest = (
        cargo_workspace if cargo_workspace.is_file() else cargo_workspace / "Cargo.toml"
    )
    cargo_lock = cargo_cwd / "Cargo.lock"
    if not cargo_manifest.is_file() or not cargo_lock.is_file():
        raise ContractError("cargo workspace must contain Cargo.toml and Cargo.lock")
    overlay_root = repo_root / "rust/agent-runtime-overlay"
    overlay_workspace = overlay_root / "kernel-rs"
    for relative in (
        "Cargo.toml",
        "Cargo.lock",
        "ai-platform-capability-contract/Cargo.toml",
        "ai-platform-capability-worker/Cargo.toml",
    ):
        composed_path = cargo_cwd / relative
        overlay_path = overlay_workspace / relative
        if (
            not composed_path.is_file()
            or not overlay_path.is_file()
            or sha256_file(composed_path) != sha256_file(overlay_path)
        ):
            raise ContractError(f"cargo workspace does not contain the current overlay {relative}")
    if cargo_metadata is not None:
        # Existing Docker source-evidence exports dependency metadata alongside
        # its exact lockfile. Source-only overlay edits do not change this graph.
        evidence_lock = cargo_metadata.parent / "Cargo.lock"
        if not evidence_lock.is_file() or sha256_file(evidence_lock) != sha256_file(cargo_lock):
            raise ContractError("offline Cargo metadata must include the matching exported Cargo.lock")
        metadata = _load_object(cargo_metadata, label="Docker Cargo metadata")
    else:
        metadata = json.loads(
            _run(["cargo", "metadata", "--locked", "--format-version", "1"], cwd=cargo_cwd)
        )
    if not isinstance(metadata, dict):
        raise ContractError("cargo metadata must return a JSON object")

    overlay = overlay_identity(overlay_root)
    overlay_lock_sha = sha256_file(overlay_root / "kernel-rs/Cargo.lock")
    schema_sha = sha256_file(repo_root / CAPABILITY_WORKER_SCHEMA_REL)
    source = receipt.get("source") or {}
    upstream_sha = _require_hex(source.get("upstream_sha"), length=40, label="source upstream SHA")
    fork_sha = _require_hex(source.get("fork_sha"), length=40, label="source fork SHA")
    manifest_path = overlay_root / "manifest.json"
    manifest = _load_object(manifest_path, label="Agent Runtime overlay manifest")
    manifest.update(
        {
            "upstream_sha": upstream_sha,
            "source_revision": fork_sha,
            "file_count": overlay["file_count"],
            "sha256": overlay["sha256"],
            "cargo_lock_sha256": overlay_lock_sha,
        }
    )

    capability_sbom_rel = build.get("capability_worker_sbom") or CAPABILITY_WORKER_SBOM_REL
    if not isinstance(capability_sbom_rel, str) or not capability_sbom_rel:
        raise ContractError("capability worker SBOM path is required")
    capability_sbom_path = repo_root / capability_sbom_rel
    capability_sbom = _capability_worker_sbom(
        metadata,
        overlay=overlay,
        schema_sha=schema_sha,
        cargo_lock_sha=overlay_lock_sha,
        source=source,
    )
    _write_object_atomic(manifest_path, manifest)
    _write_object_atomic(capability_sbom_path, capability_sbom)

    receipt["overlay"] = {
        "sha256": overlay["sha256"],
        "file_count": overlay["file_count"],
        "cargo_lock_sha256": overlay_lock_sha,
        "source_revision": fork_sha,
        "upstream_sha": upstream_sha,
    }
    if manifest.get("schema_version") == "ai-platform/agent-runtime-overlay/v2":
        manifest_digest = sha256_file(manifest_path)
        receipt["overlay"]["manifest_schema"] = manifest["schema_version"]
        receipt["overlay"]["manifest_sha256"] = manifest_digest
        build["overlay_manifest_schema"] = manifest["schema_version"]
        build["overlay_manifest_sha256"] = manifest_digest
    receipt["capability_worker_schema_sha256"] = schema_sha
    receipt["capability_worker_sbom"] = {
        "format": "CycloneDX-1.5",
        "path": capability_sbom_rel,
        "sha256": sha256_file(capability_sbom_path),
        "component_count": len(capability_sbom["components"]),
    }
    _write_object_atomic(receipt_path, receipt)

    lock_source = lock.get("source") or {}
    lock_source.update(
        {
            "upstream_url": source.get("upstream_url"),
            "upstream_sha": upstream_sha,
            "fork_sha": fork_sha,
            "git_tree_sha": source.get("git_tree_sha"),
        }
    )
    lock["source"] = lock_source
    build.update(
        {
            "overlay_manifest": OVERLAY_MANIFEST_REL,
            "overlay_sha256": overlay["sha256"],
            "overlay_file_count": overlay["file_count"],
            "overlay_cargo_lock_sha256": overlay_lock_sha,
            "capability_worker_schema_sha256": schema_sha,
            "capability_worker_sbom": capability_sbom_rel,
            "capability_worker_sbom_sha256": sha256_file(capability_sbom_path),
            "source_receipt_sha256": sha256_file(receipt_path),
        }
    )
    lock["build"] = build
    lock["release_state"] = "local_source_locked"
    artifacts = (lock.get("oci") or {}).get("artifacts") or {}
    for artifact_id, binary in ARTIFACT_BINARIES.items():
        artifact = artifacts.get(artifact_id) or {}
        artifacts[artifact_id] = {
            "binary": binary,
            "protocol": artifact.get("protocol"),
            "candidate_start_allowed": False,
            "image_ref": None,
            "image_digest": None,
            "platforms": artifact.get("platforms", ["linux/arm64"]),
        }
    lock["oci"] = {"artifacts": artifacts}
    _write_object_atomic(lock_path, lock)
    validate_lock(repo_root=repo_root, lock_path=lock_path)


def _checked_image_identity(
    lock: dict[str, Any],
    artifact_id: str,
    image_info: dict[str, Any],
) -> tuple[str, str]:
    labels = (image_info.get("Config") or {}).get("Labels") or {}
    source = lock["source"]
    build = lock["build"]
    expected_binary = ARTIFACT_BINARIES[artifact_id]
    expected_revision = source["fork_sha"]
    if build.get("overlay_sha256"):
        expected_revision = f"{source['upstream_sha']}+{build['overlay_sha256'][:12]}"
    if artifact_id == "capability_worker":
        expected_labels = {
            "org.opencontainers.image.revision": expected_revision,
            "com.misaya.ai-platform.capability-worker.schema-sha256": build[
                "capability_worker_schema_sha256"
            ],
            "com.misaya.ai-platform.capability-worker.artifact": artifact_id,
            "com.misaya.ai-platform.capability-worker.binary": expected_binary,
        }
    else:
        expected_labels = {
            "org.opencontainers.image.revision": expected_revision,
            "com.misaya.ai-platform.agent-runtime.schema-sha256": build["app_server_schema_sha256"],
            "com.misaya.ai-platform.agent-runtime.artifact": artifact_id,
            "com.misaya.ai-platform.agent-runtime.binary": expected_binary,
        }
    if artifact_id == "capability_worker" and build.get("overlay_sha256"):
        expected_labels["com.misaya.ai-platform.capability-worker.overlay-sha256"] = build[
            "overlay_sha256"
        ]
    if any(labels.get(key) != value for key, value in expected_labels.items()):
        raise ContractError("Docker image labels do not match the locked source/artifact identity")
    image_digest = image_info.get("Id")
    if not isinstance(image_digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", image_digest):
        raise ContractError("Docker image ID must be an immutable sha256 digest")
    os_name = image_info.get("Os")
    architecture = image_info.get("Architecture")
    if not all(isinstance(value, str) and value for value in (os_name, architecture)):
        raise ContractError("Docker image platform is missing")

    return image_digest, f"{os_name}/{architecture}"


def verify_local_images(
    *,
    repo_root: Path,
    lock_path: Path,
    runtime_image: str | None = None,
    worker_image: str | None = None,
) -> None:
    """Check the actual selected Runtime/Worker pair without rewriting its lock."""
    for artifact_id in ("agent_runtime", "capability_worker"):
        validate_lock(repo_root=repo_root, lock_path=lock_path, required_artifact=artifact_id)
    lock = _load_object(lock_path, label="Agent Harness lock")
    source, build = lock["source"], lock["build"]
    suffix = f"local-{source['upstream_sha'][:12]}-{build.get('overlay_sha256', '')[:12]}"
    selected = {
        "agent_runtime": runtime_image or f"ai-gateway-agent-runtime:{suffix}",
        "capability_worker": worker_image or f"ai-gateway-agent-capability-worker:{suffix}",
    }
    platforms = set()
    for artifact_id, image in selected.items():
        inspected = json.loads(_run(["docker", "image", "inspect", image], cwd=repo_root))
        if (
            not isinstance(inspected, list)
            or len(inspected) != 1
            or not isinstance(inspected[0], dict)
        ):
            raise ContractError("Docker image inspection returned an unexpected shape")
        info = inspected[0]
        digest, platform = _checked_image_identity(lock, artifact_id, info)
        artifact = lock["oci"]["artifacts"][artifact_id]
        if lock["release_state"] == "published":
            if artifact["image_ref"] not in (info.get("RepoDigests") or []):
                raise ContractError(f"{artifact_id} registry digest does not match its lock")
        elif digest != artifact["image_digest"]:
            raise ContractError(f"{artifact_id} image digest does not match its lock")
        if platform not in artifact["platforms"]:
            raise ContractError(f"{artifact_id} platform does not match its lock")
        platforms.add(platform)
    if len(platforms) != 1:
        raise ContractError("Runtime and capability worker must target the same platform")


def record_local_image(
    *,
    repo_root: Path,
    lock_path: Path,
    artifact_id: str,
    image: str,
) -> None:
    if artifact_id not in ARTIFACT_BINARIES:
        raise ContractError(f"unknown OCI artifact: {artifact_id}")
    validate_lock(repo_root=repo_root, lock_path=lock_path)
    lock = _load_object(lock_path, label="Agent Harness lock")
    inspected = json.loads(_run(["docker", "image", "inspect", image], cwd=repo_root))
    if not isinstance(inspected, list) or len(inspected) != 1 or not isinstance(inspected[0], dict):
        raise ContractError("Docker image inspection returned an unexpected shape")
    image_info = inspected[0]
    image_digest, platform = _checked_image_identity(lock, artifact_id, image_info)

    artifact = lock["oci"]["artifacts"][artifact_id]
    artifact.update(
        {
            "candidate_start_allowed": True,
            "image_ref": f"local-image://{image_digest}",
            "image_digest": image_digest,
            "platforms": [platform],
        }
    )
    lock["release_state"] = "local_image_locked"
    _write_object_atomic(lock_path, lock)
    validate_lock(
        repo_root=repo_root,
        lock_path=lock_path,
        required_artifact=artifact_id,
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    generate = subparsers.add_parser(
        "generate", help="generate source receipt and deterministic SBOM"
    )
    generate.add_argument("--fork", type=Path, required=True)
    generate.add_argument("--schema-dir", type=Path, required=True)
    generate.add_argument("--receipt", type=Path, required=True)
    generate.add_argument("--sbom", type=Path, required=True)

    composed = subparsers.add_parser("generate-composed", help="generate verified source-only composed evidence offline")
    for name in ("package", "upstream", "composed", "repo-root", "export-evidence", "schema-dir", "output"):
        composed.add_argument("--" + name, type=Path, required=True)

    refresh = subparsers.add_parser(
        "refresh-source-lock",
        help="refresh source/build/license identity and invalidate every OCI artifact",
    )
    refresh.add_argument("--repo-root", type=Path, required=True)
    refresh.add_argument("--lock", type=Path, required=True)

    refresh_overlay_parser = subparsers.add_parser(
        "refresh-overlay",
        help="refresh overlay identities and the deterministic capability-worker SBOM",
    )
    refresh_overlay_parser.add_argument("--repo-root", type=Path, required=True)
    refresh_overlay_parser.add_argument("--lock", type=Path, required=True)
    refresh_overlay_parser.add_argument("--cargo-workspace", type=Path, required=True)
    refresh_overlay_parser.add_argument("--cargo-metadata", type=Path, help="Docker-exported offline metadata with its sibling Cargo.lock")

    record = subparsers.add_parser(
        "record-local-image",
        help="record one locally built, label-verified OCI artifact",
    )
    record.add_argument("--repo-root", type=Path, required=True)
    record.add_argument("--lock", type=Path, required=True)
    record.add_argument("--artifact", choices=sorted(ARTIFACT_BINARIES), required=True)
    record.add_argument("--image", required=True)

    verify_pair = subparsers.add_parser(
        "verify-local-images",
        help="verify the selected Runtime/Worker release unit against the lock",
    )
    verify_pair.add_argument("--repo-root", type=Path, required=True)
    verify_pair.add_argument("--lock", type=Path, required=True)
    verify_pair.add_argument("--runtime-image")
    verify_pair.add_argument("--worker-image")

    validate = subparsers.add_parser("validate", help="validate the committed immutable lock")
    validate.add_argument("--repo-root", type=Path, required=True)
    validate.add_argument("--lock", type=Path, required=True)
    validate.add_argument("--require-runnable", action="store_true")
    validate.add_argument("--require-artifact", choices=sorted(ARTIFACT_BINARIES))
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        if args.command == "generate":
            generate_receipt(
                fork=args.fork,
                schema_dir=args.schema_dir,
                receipt_path=args.receipt,
                sbom_path=args.sbom,
            )
        elif args.command == "generate-composed":
            generate_composed_receipt(package=args.package, upstream=args.upstream, composed=args.composed,
                repo_root=args.repo_root, export_evidence=args.export_evidence,
                schema_dir=args.schema_dir, output=args.output)
        elif args.command == "refresh-source-lock":
            refresh_source_lock(
                repo_root=args.repo_root.resolve(),
                lock_path=args.lock.resolve(),
            )
        elif args.command == "refresh-overlay":
            refresh_overlay(
                repo_root=args.repo_root.resolve(),
                lock_path=args.lock.resolve(),
                cargo_workspace=args.cargo_workspace,
                cargo_metadata=args.cargo_metadata,
            )
        elif args.command == "verify-local-images":
            verify_local_images(
                repo_root=args.repo_root.resolve(),
                lock_path=args.lock.resolve(),
                runtime_image=args.runtime_image,
                worker_image=args.worker_image,
            )
        elif args.command == "record-local-image":
            record_local_image(
                repo_root=args.repo_root.resolve(),
                lock_path=args.lock.resolve(),
                artifact_id=args.artifact,
                image=args.image,
            )
        else:
            validate_lock(
                repo_root=args.repo_root.resolve(),
                lock_path=args.lock.resolve(),
                require_runnable=args.require_runnable,
                required_artifact=args.require_artifact,
            )
    except (ContractError, subprocess.CalledProcessError) as exc:
        print(f"agent-runtime-source-contract: {exc}", file=sys.stderr)
        return 1
    print("agent-runtime-source-contract: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
