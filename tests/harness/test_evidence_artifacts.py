from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

import pytest

from scripts.evidence import artifacts


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    ).stdout.strip()


def _repo(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "repo"
    (root / "deploy/evidence").mkdir(parents=True)
    policy = {
        "schema_version": artifacts.POLICY_SCHEMA,
        "manifest": "deploy/evidence/manifest.json",
        "scratch_roots": [{"path": "tmp/browser", "min_age_seconds": 0}],
        "durable_roots": ["reports/evidence"],
        "reference_globs": ["deploy/runbooks/**/feature-oracle.json"],
        "forbidden_path_parts": [".env", ".playwright", "auth", "storage-state"],
        "restricted_suffixes": [".har", ".trace", ".zip", ".webm", ".mp4", ".log"],
        "cleanup": {
            "default": "dry-run",
            "apply_requires_authorization_manifest": True,
            "apply_requires_external_quarantine": True,
        },
    }
    policy_path = root / "deploy/evidence/policy.json"
    policy_path.write_text(json.dumps(policy), encoding="utf-8")
    (root / "deploy/evidence/manifest.json").write_text(
        json.dumps(
            {
                "schema_version": artifacts.MANIFEST_SCHEMA,
                "policy": "deploy/evidence/policy.json",
                "entries": [],
            }
        ),
        encoding="utf-8",
    )
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "evidence@example.invalid")
    _git(root, "config", "user.name", "Evidence Test")
    _git(root, "add", "deploy")
    _git(root, "commit", "-qm", "policy")
    return root, policy_path


def test_empty_manifest_and_untracked_scratch_are_truthfully_classified(
    tmp_path: Path,
) -> None:
    root, policy_path = _repo(tmp_path)
    target = root / "tmp/browser/run/screenshot.png"
    target.parent.mkdir(parents=True)
    target.write_bytes(b"png")
    policy = artifacts.validate_policy(root, policy_path)

    assert artifacts.validate_manifest(root, policy)["entries"] == 0
    record = artifacts.classify(root, policy, "tmp/browser/run/screenshot.png", time.time())

    assert record["eligible"] is True
    assert record["bytes"] == 3


def test_cleanup_rejects_tracked_referenced_auth_restricted_and_symlinked_files(
    tmp_path: Path,
) -> None:
    root, policy_path = _repo(tmp_path)
    scratch = root / "tmp/browser"
    scratch.mkdir(parents=True)
    tracked = scratch / "tracked.png"
    tracked.write_bytes(b"tracked")
    _git(root, "add", "-f", "tmp/browser/tracked.png")
    _git(root, "commit", "-qm", "tracked evidence")
    referenced = scratch / "referenced.png"
    referenced.write_bytes(b"reference")
    oracle = root / "deploy/runbooks/example/feature-oracle.json"
    oracle.parent.mkdir(parents=True)
    oracle.write_text(
        json.dumps({"evidence": "tmp/browser/referenced.png"}), encoding="utf-8"
    )
    auth = scratch / ".env.local"
    auth.write_text("SECRET=never-read", encoding="utf-8")
    restricted = scratch / "raw.har"
    restricted.write_text("raw", encoding="utf-8")
    trace_bundle = scratch / "trace.zip"
    trace_bundle.write_bytes(b"raw trace")
    link = scratch / "linked.png"
    link.symlink_to(referenced)
    policy = artifacts.validate_policy(root, policy_path)

    records = {
        name: artifacts.classify(root, policy, f"tmp/browser/{name}", time.time())
        for name in (
            "tracked.png",
            "referenced.png",
            ".env.local",
            "raw.har",
            "trace.zip",
            "linked.png",
        )
    }

    assert "committed/tracked" in records["tracked.png"]["reasons"]
    assert "referenced evidence" in records["referenced.png"]["reasons"]
    assert any("forbidden path" in reason for reason in records[".env.local"]["reasons"])
    assert "restricted raw evidence suffix" in records["raw.har"]["reasons"]
    assert "restricted raw evidence suffix" in records["trace.zip"]["reasons"]
    assert "symlink" in records["linked.png"]["reasons"]
    assert not any(record["eligible"] for record in records.values())


def test_cleanup_defaults_to_dry_run_and_apply_requires_exact_authorization(
    tmp_path: Path,
) -> None:
    root, policy_path = _repo(tmp_path)
    target = root / "tmp/browser/run/output.png"
    target.parent.mkdir(parents=True)
    target.write_bytes(b"output")
    output = tmp_path / "dry-run.json"

    assert artifacts.command(
        [
            "cleanup",
            "--repo-root",
            str(root),
            "--policy",
            str(policy_path),
            "--path",
            "tmp/browser/run/output.png",
            "--output",
            str(output),
        ]
    ) == 0
    assert target.is_file()
    assert json.loads(output.read_text())["result"] == "dry-run"

    authorization = tmp_path / "authorization.json"
    authorization.write_text(
        json.dumps(
            {
                "schema_version": artifacts.AUTH_SCHEMA,
                "approved_by": "user",
                "run_id": "test-run",
                "artifacts": [
                    {
                        "path": "tmp/browser/run/output.png",
                        "sha256": artifacts._sha(target),
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    quarantine = tmp_path / "quarantine"
    quarantine.mkdir()
    assert artifacts.command(
        [
            "cleanup",
            "--repo-root",
            str(root),
            "--policy",
            str(policy_path),
            "--path",
            "tmp/browser/run/output.png",
            "--apply",
            "--authorization",
            str(authorization),
            "--quarantine",
            str(quarantine),
        ]
    ) == 0
    assert not target.exists()
    assert (quarantine / "test-run/tmp/browser/run/output.png").read_bytes() == b"output"


def test_cleanup_refuses_repository_root_even_if_policy_is_tampered(tmp_path: Path) -> None:
    root, policy_path = _repo(tmp_path)
    policy = json.loads(policy_path.read_text())
    policy["scratch_roots"] = [{"path": ".", "min_age_seconds": 0}]
    policy_path.write_text(json.dumps(policy), encoding="utf-8")

    try:
        artifacts.validate_policy(root, policy_path)
    except artifacts.EvidenceError as exc:
        assert "unsafe scratch root" in str(exc)
    else:
        raise AssertionError("repository-root cleanup policy was accepted")


def test_policy_cannot_weaken_raw_or_cleanup_protections(tmp_path: Path) -> None:
    root, policy_path = _repo(tmp_path)
    policy = json.loads(policy_path.read_text())
    policy["restricted_suffixes"].remove(".zip")
    policy_path.write_text(json.dumps(policy), encoding="utf-8")

    with pytest.raises(artifacts.EvidenceError, match="restricted-raw"):
        artifacts.validate_policy(root, policy_path)

    policy["restricted_suffixes"].append(".zip")
    policy["cleanup"]["default"] = "apply"
    policy_path.write_text(json.dumps(policy), encoding="utf-8")
    with pytest.raises(artifacts.EvidenceError, match="dry-run"):
        artifacts.validate_policy(root, policy_path)


def test_manifest_binds_policy_and_rejects_scratch_oracle_claim(tmp_path: Path) -> None:
    root, policy_path = _repo(tmp_path)
    policy = artifacts.validate_policy(root, policy_path)
    manifest_path = root / "deploy/evidence/manifest.json"
    manifest = json.loads(manifest_path.read_text())
    manifest["policy"] = "deploy/evidence/other-policy.json"
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    with pytest.raises(artifacts.EvidenceError, match="not bound"):
        artifacts.validate_manifest(root, policy)

    manifest["policy"] = "deploy/evidence/policy.json"
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    oracle = root / "deploy/runbooks/example/feature-oracle.json"
    oracle.parent.mkdir(parents=True)
    oracle.write_text(
        json.dumps({"evidence": "tmp/browser/run/screenshot.png"}), encoding="utf-8"
    )
    with pytest.raises(artifacts.EvidenceError, match="non-portable scratch"):
        artifacts.validate_manifest(root, policy)


def test_rejected_symlink_is_not_followed_or_hashed(
    tmp_path: Path,
    monkeypatch,
) -> None:
    root, policy_path = _repo(tmp_path)
    external = tmp_path / "external-secret"
    external.write_text("must-not-read", encoding="utf-8")
    link = root / "tmp/browser/external.png"
    link.parent.mkdir(parents=True)
    link.symlink_to(external)
    monkeypatch.setattr(
        artifacts,
        "_sha",
        lambda _path: (_ for _ in ()).throw(AssertionError("rejected path was hashed")),
    )

    record = artifacts.classify(
        root,
        artifacts.validate_policy(root, policy_path),
        "tmp/browser/external.png",
        time.time(),
    )

    assert record["eligible"] is False
    assert record["reasons"] == ["symlink"]
    assert record["sha256"] is None


def test_durable_manifest_rejects_dirty_tracked_artifact(tmp_path: Path) -> None:
    root, policy_path = _repo(tmp_path)
    artifact = root / "reports/evidence/release.json"
    artifact.parent.mkdir(parents=True)
    artifact.write_text("evidence", encoding="utf-8")
    _git(root, "add", "reports/evidence/release.json")
    _git(root, "commit", "-qm", "durable evidence")
    source_sha = _git(root, "rev-parse", "HEAD")
    manifest = {
        "schema_version": artifacts.MANIFEST_SCHEMA,
        "policy": "deploy/evidence/policy.json",
        "entries": [
            {
                "id": "release",
                "owner": "release",
                "evidence_tier": "L3",
                "media_type": "application/json",
                "bytes": artifact.stat().st_size,
                "generated_at": "2026-08-30T12:00:00Z",
                "retention": "permanent",
                "source_git_sha": source_sha,
                "command": "make platform-release-gate",
                "scenario": "release",
                "sha256": artifacts._sha(artifact),
                "viewport": None,
                "redaction_reviewer": "release-owner",
                "access_policy": "repository",
                "repository_path": "reports/evidence/release.json",
            }
        ],
    }
    (root / "deploy/evidence/manifest.json").write_text(
        json.dumps(manifest), encoding="utf-8"
    )
    policy = artifacts.validate_policy(root, policy_path)
    assert artifacts.validate_manifest(root, policy)["entries"] == 1

    artifact.write_text("drifted", encoding="utf-8")
    manifest["entries"][0]["bytes"] = artifact.stat().st_size
    manifest["entries"][0]["sha256"] = artifacts._sha(artifact)
    (root / "deploy/evidence/manifest.json").write_text(
        json.dumps(manifest), encoding="utf-8"
    )
    try:
        artifacts.validate_manifest(root, policy)
    except artifacts.EvidenceError as exc:
        assert "committed and clean" in str(exc)
    else:
        raise AssertionError("dirty tracked evidence was accepted as committed")
