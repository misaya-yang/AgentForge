#!/usr/bin/env python3
"""Generate and validate the unified ARC-08 compatibility manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Any

try:
    from scripts.release.release_evidence import (
        ReleaseEvidenceError,
        validate_integration_receipt,
        validate_release_matrix,
        validate_retirement_manifest,
    )
except ModuleNotFoundError:  # direct ``python scripts/release/...`` Make target
    from release_evidence import (  # type: ignore[no-redef]
        ReleaseEvidenceError,
        validate_integration_receipt,
        validate_release_matrix,
        validate_retirement_manifest,
    )

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "deploy/release/compatibility-manifest.json"
SCHEMA = "ai-platform/compatibility-manifest/v1"
HTTP_VERSION_SERVICES = frozenset(
    {
        "gateway",
        "knowledge-service",
    }
)
CONTAINER_IDENTITY_SERVICES = frozenset(
    {
        "frontend",
        "knowledge-worker",
        "migrator",
        "agent-runtime",
        "capability-worker",
    }
)
APP_CONTAINER_SERVICES = frozenset({"frontend", "knowledge-worker", "migrator"})
RUST_CONTAINER_SERVICES = frozenset({"agent-runtime", "capability-worker"})
SERVICES = HTTP_VERSION_SERVICES | CONTAINER_IDENTITY_SERVICES
RECEIPTS = {
    "platform_db",
    "agent_execution",
    "knowledge",
    "fresh_install",
    "rollback",
    "version_agreement",
}
RECEIPT_GATES = {
    "platform_db": "platform-db",
    "agent_execution": "agent-execution",
    "knowledge": "knowledge",
    "fresh_install": "fresh-install",
    "rollback": "rollback",
    "version_agreement": "version-agreement",
}
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
IMAGE_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
RELEASE_ID = re.compile(r"^[a-z0-9][a-z0-9._-]{7,127}$")


class ManifestError(RuntimeError):
    pass


def _load(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise ManifestError(f"{label} unreadable: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise ManifestError(f"{label} must be an object")
    return value


def _sha(path: Path) -> str | None:
    if not path.is_file() or path.is_symlink():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _canonical_sha(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    ).hexdigest()


def _git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(["git", *args], cwd=root, capture_output=True, text=True, check=False)
    if check and result.returncode != 0:
        raise ManifestError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def _lock(root: Path) -> dict[str, Any]:
    return _load(root / "deploy/agent-runtime-source/lock.json", "Runtime source lock")


def _database(root: Path) -> dict[str, Any]:
    baseline_id = "2026_08_post_kb_v1"
    baseline = root / f"database/baselines/{baseline_id}/manifest.json"
    epoch = root / f"database/migrations/{baseline_id}/manifest.yml"
    roles = root / "database/bootstrap/roles.sql"
    grants = root / f"database/baselines/{baseline_id}/grants.sql"
    roles_sha = _sha(roles)
    grants_sha = _sha(grants)
    grants_revision = (
        _canonical_sha({"roles": roles_sha, "grants": grants_sha})
        if roles_sha and grants_sha
        else None
    )
    return {
        "baseline_id": baseline_id,
        "baseline_manifest_sha256": _sha(baseline),
        "migration_revision": _sha(epoch),
        "grants_revision": grants_revision,
    }


def _compose_revision(root: Path, files: tuple[str, ...]) -> str | None:
    digests = {name: _sha(root / name) for name in files}
    if any(digest is None for digest in digests.values()):
        return None
    return _canonical_sha(digests)


def _release_evidence(root: Path) -> dict[str, Any]:
    files = {
        "matrix": "deploy/release/release-rollback-matrix.json",
        "integration_gates": "deploy/release/integration-gates.json",
        "retirement_manifest": "deploy/release/historical-plan-retirement.json",
        "matrix_schema": "deploy/release/schemas/release-rollback-matrix-v1.schema.json",
        "receipt_schema": "deploy/release/schemas/integration-gate-receipt-v1.schema.json",
        "retirement_schema": "deploy/release/schemas/historical-plan-retirement-v1.schema.json",
        "closeout_template": "deploy/release/FINAL-CLOSEOUT-TEMPLATE.md",
    }
    return {f"{name}_path": path for name, path in files.items()} | {
        f"{name}_sha256": _sha(root / path) for name, path in files.items()
    }


def build_offline(root: Path) -> dict[str, Any]:
    runtime = _lock(root)
    source = runtime.get("source") if isinstance(runtime.get("source"), dict) else {}
    build = runtime.get("build") if isinstance(runtime.get("build"), dict) else {}
    rust = str(build.get("rustc") or "")
    rust_match = re.match(r"^rustc ([0-9]+\.[0-9]+\.[0-9]+)", rust)
    bm25_source = (
        root / "apps/knowledge-service/src/knowledge_service/services/knowledge/lexical_config.py"
    )
    bm25_text = bm25_source.read_text(encoding="utf-8") if bm25_source.is_file() else ""
    bm25_match = re.search(r'^BM25_V2_ENCODER_CONTRACT_VERSION\s*=\s*"([^"]+)"', bm25_text, re.M)
    return {
        "runtime_overlay": {
            "lock": "deploy/agent-runtime-source/lock.json",
            "lock_sha256": _sha(root / "deploy/agent-runtime-source/lock.json"),
            "upstream_sha": source.get("upstream_sha"),
            "overlay_sha256": build.get("overlay_sha256"),
            "runtime_schema_digest": build.get("app_server_schema_sha256"),
            "capability_worker_schema_digest": build.get("capability_worker_schema_sha256"),
        },
        "database": _database(root),
        "contracts": {
            "openapi_sha256": _sha(root / "sdk/openapi.json"),
            "sse_fixture_sha256": _sha(root / "sdk/fixtures/sse_inner_envelopes.json"),
            "capability_contract_sha256": _sha(
                root
                / "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lib.rs"
            ),
            "agent_event_schema_sha256": _sha(
                root / "packages/ai-gateway-contracts/src/ai_gateway_contracts/event_envelope.py"
            ),
        },
        "topology": {
            "compact_profile_revision": _compose_revision(
                root, ("docker-compose.yml", "docker-compose.compact.yml")
            ),
            "scale_profile_revision": _compose_revision(
                root, ("docker-compose.yml", "docker-compose.scale.yml")
            ),
            "service_topology_revision": _sha(root / "src/core/data/service_topology.json"),
            "data_access_revision": _sha(
                root / "docs/architecture/baselines/2026-08-post-rag/data-access-inventory.json"
            ),
            "quality_baseline_revision": _sha(
                root / "docs/architecture/baselines/2026-08-post-rag/loc-baseline.json"
            ),
            "evidence_policy_revision": _sha(root / "deploy/evidence/policy.json"),
        },
        "toolchains": {
            "python_major": "3",
            "node_major": "26",
            "rust": rust_match.group(1) if rust_match else None,
        },
        "release_evidence": _release_evidence(root),
        "vector_contract": {
            "bm25_revision": bm25_match.group(1) if bm25_match else None,
        },
    }


def missing_candidate_fields(root: Path, manifest: dict[str, Any]) -> list[str]:
    missing: list[str] = []
    if not isinstance(manifest.get("release_id"), str):
        missing.append("release_id")
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    for field in ("git_sha", "git_tree_sha"):
        if not isinstance(source.get(field), str):
            missing.append(f"source.{field}")
    services = manifest.get("services") if isinstance(manifest.get("services"), dict) else {}
    for service in sorted(SERVICES):
        record = services.get(service) if isinstance(services.get(service), dict) else {}
        if not record.get("image_digest"):
            missing.append(f"services.{service}.image_digest")
        identity = record.get("identity") if isinstance(record.get("identity"), dict) else {}
        if not identity:
            missing.append(f"services.{service}.identity")
            continue
        for field in ("mode", "service_id", "release_id", "git_sha", "image_digest"):
            if not identity.get(field):
                missing.append(f"services.{service}.identity.{field}")
        if service in APP_CONTAINER_SERVICES and not identity.get("image_version"):
            missing.append(f"services.{service}.identity.image_version")
    vectors = manifest.get("vectors") if isinstance(manifest.get("vectors"), dict) else {}
    for field in (
        "qdrant_dataset_revision",
        "memory_namespace_revision",
        "collection_or_alias",
        "embedding_provider",
        "embedding_model",
        "embedding_dimension",
        "bm25_revision",
    ):
        if vectors.get(field) in (None, ""):
            missing.append(f"vectors.{field}")
    receipts = manifest.get("receipts") if isinstance(manifest.get("receipts"), dict) else {}
    for receipt in sorted(RECEIPTS):
        if not receipts.get(receipt):
            missing.append(f"receipts.{receipt}")
    expected = build_offline(root)
    for section in (
        "runtime_overlay",
        "database",
        "contracts",
        "topology",
        "toolchains",
        "release_evidence",
    ):
        actual = manifest.get(section) if isinstance(manifest.get(section), dict) else {}
        for field, current in expected[section].items():
            if current in (None, "") or actual.get(field) in (None, ""):
                missing.append(f"{section}.{field}")
    bm25 = expected["vector_contract"].get("bm25_revision")
    if bm25 in (None, ""):
        missing.append("vectors.bm25_revision")
    return list(dict.fromkeys(missing))


def _compare_offline(manifest: dict[str, Any], expected: dict[str, Any]) -> None:
    for section in (
        "runtime_overlay",
        "database",
        "contracts",
        "topology",
        "toolchains",
        "release_evidence",
    ):
        actual = manifest.get(section)
        if not isinstance(actual, dict):
            raise ManifestError(f"manifest section missing: {section}")
        for key, value in expected[section].items():
            if actual.get(key) not in (None, value):
                raise ManifestError(
                    f"offline compatibility value drift: {section}.{key}: "
                    f"manifest={actual.get(key)!r} current={value!r}"
                )
    vectors = manifest.get("vectors")
    if not isinstance(vectors, dict):
        raise ManifestError("manifest section missing: vectors")
    bm25 = expected["vector_contract"]["bm25_revision"]
    if vectors.get("bm25_revision") not in (None, bm25):
        raise ManifestError("vectors.bm25_revision drift")


def _receipt_path(root: Path, raw: str, name: str) -> Path:
    if "\\" in raw:
        raise ManifestError(f"receipt path is not a repository POSIX path: {name}")
    rel = PurePosixPath(raw)
    if rel.is_absolute() or ".." in rel.parts or rel.as_posix() != raw:
        raise ManifestError(f"receipt path escapes repository: {name}")
    if not (raw.startswith("reports/") or raw.startswith("tmp/gate-evidence/")):
        raise ManifestError(f"receipt path is outside evidence roots: {name}")
    path = root / raw
    if path.is_symlink() or not path.is_file():
        raise ManifestError(f"receipt is missing or symlinked: {name}")
    return path


def _verify_receipts(root: Path, manifest: dict[str, Any]) -> None:
    release_id = manifest["release_id"]
    source_sha = manifest["source"]["git_sha"]
    for name, raw in manifest["receipts"].items():
        if not isinstance(raw, str):
            raise ManifestError(f"receipt path missing: {name}")
        path = _receipt_path(root, raw, name)
        receipt = _load(path, f"{name} receipt")
        try:
            validate_integration_receipt(
                receipt,
                expected_gate=RECEIPT_GATES[name],
                release_id=release_id,
                source_git_sha=source_sha,
                require_pass=True,
            )
        except ReleaseEvidenceError as exc:
            raise ManifestError(f"receipt is not a zero-skip pass: {name}: {exc}") from exc


def _validate_service_identities(manifest: dict[str, Any], *, require_complete: bool) -> None:
    release_id = manifest.get("release_id")
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    services = manifest.get("services") if isinstance(manifest.get("services"), dict) else {}
    for service in sorted(SERVICES):
        record = services.get(service)
        if not isinstance(record, dict):
            raise ManifestError(f"service release record is invalid: {service}")
        identity = record.get("identity")
        if identity is None and not require_complete:
            continue
        if not isinstance(identity, dict):
            raise ManifestError(f"service release identity is invalid: {service}")
        expected_mode = "http_version" if service in HTTP_VERSION_SERVICES else "container_image"
        expected = {
            "mode": expected_mode,
            "service_id": service,
            "release_id": release_id,
            "git_sha": source.get("git_sha"),
            "image_digest": record.get("image_digest"),
        }
        for key, value in expected.items():
            actual = identity.get(key)
            if (require_complete or actual is not None) and actual != value:
                raise ManifestError(f"service release identity mismatch: {service}.{key}")
        if service in APP_CONTAINER_SERVICES:
            image_version = identity.get("image_version")
            if require_complete and not isinstance(image_version, str):
                raise ManifestError(f"service OCI version identity is missing: {service}")
            if image_version is not None and (
                not isinstance(image_version, str) or not image_version.strip()
            ):
                raise ManifestError(f"service OCI version identity is invalid: {service}")
        elif identity.get("image_version") is not None:
            raise ManifestError(f"service OCI version identity is unexpected: {service}")


def validate(root: Path, manifest: dict[str, Any], *, level: str) -> dict[str, Any]:
    if manifest.get("schema_version") != SCHEMA or manifest.get("status") not in {
        "draft",
        "release_candidate",
        "released",
    }:
        raise ManifestError("unsupported compatibility manifest schema/status")
    if (
        set(manifest.get("services") or {}) != SERVICES
        or set(manifest.get("receipts") or {}) != RECEIPTS
    ):
        raise ManifestError("compatibility manifest service/receipt set drift")
    _validate_service_identities(manifest, require_complete=False)
    _compare_offline(manifest, build_offline(root))
    missing = missing_candidate_fields(root, manifest)
    if level == "candidate" and (
        manifest.get("status") not in {"release_candidate", "released"} or missing
    ):
        raise ManifestError(f"candidate manifest is incomplete: {missing}")
    if level == "candidate":
        _validate_service_identities(manifest, require_complete=True)
    try:
        matrix = _load(
            root / "deploy/release/release-rollback-matrix.json", "release/rollback matrix"
        )
        retirement = _load(
            root / "deploy/release/historical-plan-retirement.json",
            "historical-plan retirement manifest",
        )
        validate_retirement_manifest(root, retirement)
        source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
        validate_release_matrix(
            root,
            matrix,
            level=level,
            release_id=manifest.get("release_id") if level == "candidate" else None,
            source_git_sha=source.get("git_sha") if level == "candidate" else None,
        )
    except ReleaseEvidenceError as exc:
        raise ManifestError(f"release evidence invalid: {exc}") from exc
    if level == "candidate":
        if RELEASE_ID.fullmatch(manifest["release_id"]) is None:
            raise ManifestError("release_id is invalid")
        source = manifest["source"]
        if (
            HEX_40.fullmatch(source["git_sha"]) is None
            or HEX_40.fullmatch(source["git_tree_sha"]) is None
        ):
            raise ManifestError("source Git identity is invalid")
        for service, record in manifest["services"].items():
            if IMAGE_DIGEST.fullmatch(record["image_digest"]) is None:
                raise ManifestError(f"service image is not digest-pinned: {service}")
        resolved = _git(root, "rev-parse", f"{source['git_sha']}^{{commit}}").stdout.strip()
        tree = _git(root, "rev-parse", f"{source['git_sha']}^{{tree}}").stdout.strip()
        if resolved != source["git_sha"] or tree != source["git_tree_sha"]:
            raise ManifestError("source Git commit/tree does not resolve exactly")
        _verify_receipts(root, manifest)
    return {
        "result": "pass" if not missing else "draft",
        "level": level,
        "status": manifest["status"],
        "missing": missing,
        "manifest_sha256": _canonical_sha(manifest),
    }


def generate_candidate(root: Path, source_rev: str, inputs: dict[str, Any]) -> dict[str, Any]:
    head = _git(root, "rev-parse", "HEAD").stdout.strip()
    dirty = _git(root, "status", "--porcelain=v1", "--untracked-files=all").stdout
    if dirty or source_rev != head or HEX_40.fullmatch(source_rev) is None:
        raise ManifestError("candidate generation requires explicit clean HEAD --source-rev")
    tree = _git(root, "rev-parse", f"{source_rev}^{{tree}}").stdout.strip()
    offline = build_offline(root)
    services = inputs.get("services")
    vectors = inputs.get("vectors")
    receipts = inputs.get("receipts")
    release_id = inputs.get("release_id")
    if (
        not isinstance(services, dict)
        or not isinstance(vectors, dict)
        or not isinstance(receipts, dict)
    ):
        raise ManifestError("candidate inputs require services, vectors, and receipts objects")
    if not isinstance(release_id, str) or RELEASE_ID.fullmatch(release_id) is None:
        raise ManifestError("candidate inputs require a valid release_id")
    payload: dict[str, Any] = {
        "schema_version": SCHEMA,
        "status": "release_candidate",
        "release_id": release_id,
        "source": {"git_sha": source_rev, "git_tree_sha": tree},
        "services": services,
        "runtime_overlay": offline["runtime_overlay"],
        "database": offline["database"],
        "contracts": offline["contracts"],
        "topology": offline["topology"],
        "toolchains": offline["toolchains"],
        "release_evidence": offline["release_evidence"],
        "vectors": {**vectors, **offline["vector_contract"]},
        "receipts": receipts,
    }
    validate(root, payload, level="candidate")
    return payload


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--level", choices=("draft", "candidate"), default="draft")
    parser.add_argument("--write", action="store_true")
    parser.add_argument("--source-rev")
    parser.add_argument("--inputs", type=Path)
    args = parser.parse_args(argv)
    try:
        root = args.repo_root.resolve()
        if args.write:
            if not args.source_rev or args.inputs is None:
                raise ManifestError("--write requires --source-rev and --inputs")
            payload = generate_candidate(
                root, args.source_rev, _load(args.inputs, "release inputs")
            )
            args.manifest.parent.mkdir(parents=True, exist_ok=True)
            args.manifest.write_text(
                json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            print(f"wrote candidate manifest: {args.manifest}")
            return 0
        result = validate(root, _load(args.manifest, "compatibility manifest"), level=args.level)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except ManifestError as exc:
        print(f"COMPATIBILITY ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
