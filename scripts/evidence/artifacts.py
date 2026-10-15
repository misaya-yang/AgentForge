#!/usr/bin/env python3
"""Validate durable evidence and safely classify/clean local browser artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import glob
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_POLICY = ROOT / "deploy/evidence/policy.json"
POLICY_SCHEMA = "ai-platform/evidence-policy/v1"
MANIFEST_SCHEMA = "ai-platform/evidence-manifest/v1"
AUTH_SCHEMA = "ai-platform/evidence-cleanup-authorization/v1"
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
GLOB_META = re.compile(r"[*?\[\]{}]")
REQUIRED_FORBIDDEN_PARTS = {".env", ".playwright", "auth", "storage-state"}
REQUIRED_RESTRICTED_SUFFIXES = {".har", ".trace", ".zip", ".webm", ".mp4", ".log"}


class EvidenceError(RuntimeError):
    pass


def _load(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise EvidenceError(f"{label} unreadable: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} must be an object: {path}")
    return value


def _sha(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _safe_rel(raw: Any, label: str) -> str:
    if not isinstance(raw, str) or not raw or "\\" in raw or GLOB_META.search(raw):
        raise EvidenceError(f"{label} must be one explicit POSIX repository path")
    value = PurePosixPath(raw)
    if value.is_absolute() or ".." in value.parts or value.as_posix() != raw:
        raise EvidenceError(f"{label} escapes the repository: {raw!r}")
    return raw


def _git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise EvidenceError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def _is_committed_clean_file(root: Path, rel: str) -> bool:
    """Return true only when ``rel`` exists unchanged in the current HEAD tree."""
    object_check = _git(root, "cat-file", "-e", f"HEAD:{rel}", check=False)
    if object_check.returncode != 0:
        return False
    return _git(root, "diff", "--quiet", "HEAD", "--", rel, check=False).returncode == 0


def _repo_identity(root: Path) -> tuple[Path, Path, list[Path]]:
    repo = Path(_git(root, "rev-parse", "--show-toplevel").stdout.strip()).resolve()
    common_raw = Path(_git(repo, "rev-parse", "--git-common-dir").stdout.strip())
    common = (repo / common_raw).resolve() if not common_raw.is_absolute() else common_raw.resolve()
    worktrees: list[Path] = []
    output = _git(repo, "worktree", "list", "--porcelain").stdout
    for line in output.splitlines():
        if line.startswith("worktree "):
            worktrees.append(Path(line.removeprefix("worktree ")).resolve())
    return repo, common, worktrees


def validate_policy(root: Path, policy_path: Path) -> dict[str, Any]:
    try:
        policy_mode = policy_path.lstat().st_mode
        resolved_policy = policy_path.resolve(strict=True)
        policy_rel = resolved_policy.relative_to(root.resolve()).as_posix()
    except (OSError, ValueError) as exc:
        raise EvidenceError(f"evidence policy must be a real repository file: {policy_path}") from exc
    if stat.S_ISLNK(policy_mode) or not stat.S_ISREG(policy_mode):
        raise EvidenceError(f"evidence policy must be a real repository file: {policy_path}")
    policy = _load(policy_path, "evidence policy")
    if policy.get("schema_version") != POLICY_SCHEMA:
        raise EvidenceError("unsupported evidence policy schema")
    scratch = policy.get("scratch_roots")
    durable = policy.get("durable_roots")
    if not isinstance(scratch, list) or not scratch or not isinstance(durable, list):
        raise EvidenceError("policy requires non-empty scratch_roots and durable_roots")
    seen: set[str] = set()
    for entry in scratch:
        if not isinstance(entry, dict):
            raise EvidenceError("scratch root entry must be an object")
        rel = _safe_rel(entry.get("path"), "scratch root")
        age = entry.get("min_age_seconds")
        if rel in {".", ".git", "tmp", "web"} or not isinstance(age, int) or age < 0:
            raise EvidenceError(f"unsafe scratch root policy: {entry!r}")
        if rel in seen:
            raise EvidenceError(f"duplicate policy root: {rel}")
        seen.add(rel)
    for raw in durable:
        rel = _safe_rel(raw, "durable root")
        if rel in seen or rel in {".", ".git", "reports"}:
            raise EvidenceError(f"unsafe/duplicate durable root: {rel}")
        seen.add(rel)
    forbidden = policy.get("forbidden_path_parts")
    if not isinstance(forbidden, list) or not REQUIRED_FORBIDDEN_PARTS.issubset(
        {str(value).lower() for value in forbidden}
    ):
        raise EvidenceError("policy weakens the required auth/environment path protections")
    restricted = policy.get("restricted_suffixes")
    if not isinstance(restricted, list) or not REQUIRED_RESTRICTED_SUFFIXES.issubset(
        {str(value).lower() for value in restricted}
    ):
        raise EvidenceError("policy weakens the required restricted-raw suffix protections")
    cleanup = policy.get("cleanup")
    if not isinstance(cleanup, dict) or cleanup != {
        "default": "dry-run",
        "apply_requires_authorization_manifest": True,
        "apply_requires_external_quarantine": True,
    }:
        raise EvidenceError("cleanup policy must remain dry-run and fail closed")
    manifest = _safe_rel(policy.get("manifest"), "manifest")
    if not (root / manifest).is_file():
        raise EvidenceError(f"evidence manifest is missing: {manifest}")
    policy["_validated_policy_path"] = policy_rel
    return policy


def _manifest_references(root: Path, policy: dict[str, Any]) -> set[str]:
    references: set[str] = set()
    manifest = _load(root / policy["manifest"], "evidence manifest")
    for entry in manifest.get("entries", []):
        if isinstance(entry, dict) and isinstance(entry.get("repository_path"), str):
            references.add(entry["repository_path"])
    for pattern in policy.get("reference_globs", []):
        if not isinstance(pattern, str):
            raise EvidenceError("reference_globs entries must be strings")
        for raw in glob.glob(str(root / pattern), recursive=True):
            path = Path(raw)
            if not path.is_file() or path.is_symlink():
                continue
            try:
                payload = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError, UnicodeDecodeError):
                continue
            text = json.dumps(payload, sort_keys=True)
            for scratch in policy["scratch_roots"]:
                prefix = f'{scratch["path"]}/'
                references.update(
                    match.group(0)
                    for match in re.finditer(rf"{re.escape(prefix)}[^\s\"';]+", text)
                )
    return references


def validate_manifest(root: Path, policy: dict[str, Any]) -> dict[str, Any]:
    path = root / policy["manifest"]
    manifest = _load(path, "evidence manifest")
    if manifest.get("schema_version") != MANIFEST_SCHEMA:
        raise EvidenceError("unsupported evidence manifest schema")
    if manifest.get("policy") != policy.get("_validated_policy_path"):
        raise EvidenceError("evidence manifest is not bound to the validated policy")
    entries = manifest.get("entries")
    if not isinstance(entries, list):
        raise EvidenceError("evidence manifest entries must be a list")
    ids: set[str] = set()
    durable_roots = tuple(f"{value}/" for value in policy["durable_roots"])
    required = {
        "id", "owner", "evidence_tier", "media_type", "bytes", "generated_at",
        "retention", "source_git_sha", "command", "scenario", "sha256",
        "viewport", "redaction_reviewer", "access_policy",
    }
    allowed = required | {"repository_path", "uri"}
    for entry in entries:
        if not isinstance(entry, dict) or not required.issubset(entry):
            raise EvidenceError("durable evidence entry is missing required fields")
        if set(entry) - allowed:
            raise EvidenceError(
                f"durable evidence entry has unknown fields: {sorted(set(entry) - allowed)}"
            )
        evidence_id = entry["id"]
        if not isinstance(evidence_id, str) or not evidence_id or evidence_id in ids:
            raise EvidenceError(f"invalid/duplicate evidence id: {evidence_id!r}")
        ids.add(evidence_id)
        if entry["evidence_tier"] not in {"L0", "L1", "L2", "L3"}:
            raise EvidenceError(f"invalid evidence tier: {evidence_id}")
        for field in (
            "owner",
            "media_type",
            "generated_at",
            "retention",
            "command",
            "scenario",
            "redaction_reviewer",
            "access_policy",
        ):
            if not isinstance(entry[field], str) or not entry[field].strip():
                raise EvidenceError(f"invalid evidence field {field}: {evidence_id}")
        try:
            generated_at = dt.datetime.fromisoformat(
                entry["generated_at"].replace("Z", "+00:00")
            )
        except ValueError as exc:
            raise EvidenceError(f"invalid generation time: {evidence_id}") from exc
        if generated_at.tzinfo is None:
            raise EvidenceError(f"evidence generation time lacks timezone: {evidence_id}")
        viewport = entry["viewport"]
        if viewport is not None and (
            not isinstance(viewport, dict)
            or not all(isinstance(viewport.get(key), int) and viewport[key] > 0 for key in ("width", "height"))
        ):
            raise EvidenceError(f"invalid evidence viewport: {evidence_id}")
        if not isinstance(entry["bytes"], int) or entry["bytes"] <= 0:
            raise EvidenceError(f"invalid evidence size: {evidence_id}")
        if HEX_40.fullmatch(str(entry["source_git_sha"])) is None or HEX_64.fullmatch(
            str(entry["sha256"])
        ) is None:
            raise EvidenceError(f"invalid evidence provenance hash: {evidence_id}")
        source = str(entry["source_git_sha"])
        resolved_source = _git(root, "rev-parse", f"{source}^{{commit}}", check=False)
        if resolved_source.returncode != 0 or resolved_source.stdout.strip() != source:
            raise EvidenceError(f"evidence source Git object is unavailable: {evidence_id}")
        if _git(root, "merge-base", "--is-ancestor", source, "HEAD", check=False).returncode != 0:
            raise EvidenceError(f"evidence source Git object is not an ancestor of HEAD: {evidence_id}")
        repository_path = entry.get("repository_path")
        uri = entry.get("uri")
        if bool(repository_path) == bool(uri):
            raise EvidenceError(f"evidence must have exactly one path or URI: {evidence_id}")
        if repository_path:
            rel = _safe_rel(repository_path, f"evidence {evidence_id} path")
            if not rel.startswith(durable_roots):
                raise EvidenceError(f"durable evidence is outside durable roots: {rel}")
            artifact = root / rel
            if artifact.is_symlink() or not artifact.is_file():
                raise EvidenceError(f"durable evidence missing/symlinked: {rel}")
            if artifact.stat().st_size != entry["bytes"] or _sha(artifact) != entry["sha256"]:
                raise EvidenceError(f"durable evidence size/hash drift: {rel}")
            if not _is_committed_clean_file(root, rel):
                raise EvidenceError(
                    f"durable repository evidence is not committed and clean: {rel}"
                )
            if str(entry["media_type"]).startswith("image/") and viewport is None:
                raise EvidenceError(f"image evidence requires a viewport: {evidence_id}")
            if entry["access_policy"] != "repository":
                raise EvidenceError(f"repository evidence access policy must be repository: {evidence_id}")
        else:
            parsed = urlsplit(uri) if isinstance(uri, str) else None
            if (
                parsed is None
                or parsed.scheme != "sealed"
                or not parsed.netloc
                or parsed.path in {"", "/"}
                or parsed.username is not None
                or parsed.password is not None
                or parsed.query
                or parsed.fragment
            ):
                raise EvidenceError(
                    f"external evidence URI must be a portable sealed://bundle/artifact URI: {evidence_id}"
                )
            if entry["access_policy"] == "repository":
                raise EvidenceError(f"sealed evidence cannot use repository access policy: {evidence_id}")

    controlled_references = _controlled_oracle_references(root, policy)
    scratch_prefixes = tuple(f"{item['path']}/" for item in policy["scratch_roots"])
    durable_paths = {
        str(entry["repository_path"])
        for entry in entries
        if isinstance(entry, dict) and entry.get("repository_path")
    }
    for source_path, reference in controlled_references:
        if reference.startswith(scratch_prefixes):
            raise EvidenceError(
                f"feature oracle references non-portable scratch evidence: {source_path}: {reference}"
            )
        if reference.startswith(durable_roots) and reference not in durable_paths:
            raise EvidenceError(
                f"feature oracle durable evidence is missing from manifest: {source_path}: {reference}"
            )
    return {"entries": len(entries), "manifest_sha256": _sha(path)}


def _controlled_oracle_references(
    root: Path, policy: dict[str, Any]
) -> list[tuple[str, str]]:
    """Return policy-controlled artifact paths named by machine feature oracles."""

    prefixes = [
        *(f"{item['path']}/" for item in policy["scratch_roots"]),
        *(f"{item}/" for item in policy["durable_roots"]),
    ]
    references: set[tuple[str, str]] = set()
    for pattern in policy.get("reference_globs", []):
        if not isinstance(pattern, str):
            raise EvidenceError("reference_globs entries must be strings")
        for raw in glob.glob(str(root / pattern), recursive=True):
            source = Path(raw)
            if not source.is_file() or source.is_symlink():
                continue
            try:
                payload = json.loads(source.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
                raise EvidenceError(f"feature oracle is unreadable: {source}: {exc}") from exc
            text = json.dumps(payload, sort_keys=True)
            for prefix in prefixes:
                for match in re.finditer(rf"{re.escape(prefix)}[^\s\"';,\]]+", text):
                    references.add((source.relative_to(root).as_posix(), match.group(0)))
    return sorted(references)


def _forbidden_content(rel: str, policy: dict[str, Any]) -> str | None:
    parts = tuple(part.lower() for part in PurePosixPath(rel).parts)
    for raw in policy.get("forbidden_path_parts", []):
        value = str(raw).lower()
        if any(part == value or part.startswith(f"{value}.") for part in parts):
            return f"forbidden path part {raw!r}"
    if Path(rel).suffix.lower() in set(policy.get("restricted_suffixes", [])):
        return "restricted raw evidence suffix"
    return None


def classify(root: Path, policy: dict[str, Any], raw: str, now: float) -> dict[str, Any]:
    rel = _safe_rel(raw, "artifact path")
    repo, common, worktrees = _repo_identity(root)
    candidate = root / rel
    reasons: list[str] = []
    try:
        file_stat = candidate.lstat()
        mode = file_stat.st_mode
    except OSError as exc:
        return {"path": rel, "eligible": False, "reasons": [f"missing/unreadable: {exc}"]}
    if stat.S_ISLNK(mode):
        return {
            "path": rel,
            "bytes": None,
            "age_seconds": max(0, int(now - file_stat.st_mtime)),
            "eligible": False,
            "reasons": ["symlink"],
            "sha256": None,
        }
    resolved = candidate.resolve(strict=False)
    if resolved == repo:
        reasons.append("repository root")
    if resolved == common or common in resolved.parents:
        reasons.append("Git common-dir")
    if any(
        worktree != repo and (resolved == worktree or worktree in resolved.parents)
        for worktree in worktrees
    ):
        reasons.append("another Git worktree")
    try:
        resolved.relative_to(repo)
    except ValueError:
        reasons.append("external path")
    if file_stat.st_dev != repo.stat().st_dev:
        reasons.append("external mount")
    allowed: dict[str, int] = {
        entry["path"]: entry["min_age_seconds"] for entry in policy["scratch_roots"]
    }
    matching = [prefix for prefix in allowed if rel.startswith(f"{prefix}/")]
    if not matching:
        reasons.append("outside scratch allowlist")
        minimum_age = 0
    else:
        minimum_age = max(allowed[prefix] for prefix in matching)
    forbidden = _forbidden_content(rel, policy)
    if forbidden:
        reasons.append(forbidden)
    if not stat.S_ISREG(mode):
        reasons.append("not a regular file")
    if _git(root, "ls-files", "--error-unmatch", "--", rel, check=False).returncode == 0:
        reasons.append("committed/tracked")
    if rel in _manifest_references(root, policy):
        reasons.append("referenced evidence")
    age = max(0, int(now - file_stat.st_mtime))
    if age < minimum_age:
        reasons.append(f"younger than {minimum_age}s")
    eligible = not reasons
    return {
        "path": rel,
        "bytes": file_stat.st_size if stat.S_ISREG(mode) else None,
        "age_seconds": age,
        "eligible": eligible,
        "reasons": reasons,
        # Never read a rejected candidate's contents. This keeps auth data,
        # restricted raw artifacts, and external mounts out of process memory.
        "sha256": _sha(candidate) if eligible and stat.S_ISREG(mode) else None,
    }


def discover(root: Path, policy: dict[str, Any]) -> list[str]:
    found: list[str] = []
    for entry in policy["scratch_roots"]:
        start = root / entry["path"]
        if not start.exists() or start.is_symlink() or not start.is_dir():
            continue
        for path in start.rglob("*"):
            if path.is_file() or path.is_symlink():
                found.append(path.relative_to(root).as_posix())
    return sorted(set(found))


def _authorization(path: Path, records: list[dict[str, Any]]) -> str:
    payload = _load(path, "cleanup authorization")
    if payload.get("schema_version") != AUTH_SCHEMA or payload.get("approved_by") != "user":
        raise EvidenceError("cleanup apply requires an explicit user authorization manifest")
    run_id = payload.get("run_id")
    if not isinstance(run_id, str) or re.fullmatch(r"[a-zA-Z0-9._-]{1,64}", run_id) is None:
        raise EvidenceError("cleanup authorization run_id is invalid")
    approved = payload.get("artifacts")
    expected = {item["path"]: item["sha256"] for item in records}
    if not isinstance(approved, list) or {
        item.get("path"): item.get("sha256") for item in approved if isinstance(item, dict)
    } != expected:
        raise EvidenceError("cleanup authorization does not exactly match eligible artifacts")
    return run_id


def _quarantine(root: Path, path: Path, worktrees: list[Path]) -> Path:
    if path.is_symlink() or not path.is_dir():
        raise EvidenceError("quarantine must be an existing real directory")
    resolved = path.resolve()
    protected = [root.resolve(), *worktrees]
    if any(resolved == item or item in resolved.parents or resolved in item.parents for item in protected):
        raise EvidenceError("quarantine must be external to every repository worktree")
    return resolved


def command(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("validate", "status", "cleanup"))
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--policy", type=Path, default=DEFAULT_POLICY)
    parser.add_argument("--path", action="append", default=[])
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--authorization", type=Path)
    parser.add_argument("--quarantine", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.apply and args.action != "cleanup":
            raise EvidenceError("--apply is valid only for cleanup")
        root = args.repo_root.resolve()
        policy = validate_policy(root, args.policy)
        manifest = validate_manifest(root, policy)
        if args.action == "validate":
            result = {"action": "validate", "result": "pass", **manifest}
        else:
            raw_paths = args.path or discover(root, policy)
            records = [classify(root, policy, raw, dt.datetime.now().timestamp()) for raw in raw_paths]
            result = {
                "action": args.action,
                "mode": "apply" if args.apply else "dry-run",
                "records": records,
                "result": "dry-run" if not args.apply else "pending",
            }
            if args.action == "cleanup" and args.apply:
                eligible = [record for record in records if record["eligible"]]
                if len(eligible) != len(records) or not eligible:
                    raise EvidenceError("cleanup apply refuses ineligible or empty selections")
                if args.authorization is None or args.quarantine is None:
                    raise EvidenceError("cleanup apply requires authorization and quarantine")
                run_id = _authorization(args.authorization, eligible)
                _, _, worktrees = _repo_identity(root)
                quarantine = _quarantine(root, args.quarantine, worktrees) / run_id
                moved: list[str] = []
                for record in eligible:
                    source = root / record["path"]
                    target = quarantine / record["path"]
                    target.parent.mkdir(parents=True, exist_ok=True)
                    try:
                        os.replace(source, target)
                    except OSError as exc:
                        raise EvidenceError(f"quarantine move failed: {record['path']}: {exc}") from exc
                    moved.append(record["path"])
                result.update({"result": "quarantined", "moved": moved})
        text = json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(text, encoding="utf-8")
        print(text, end="")
        return 0
    except EvidenceError as exc:
        print(f"EVIDENCE ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(command())
