"""Frozen baseline artifact parsing and Git provenance validation."""

from __future__ import annotations

import json
import re
import subprocess
from datetime import datetime
from pathlib import Path, PurePosixPath

import yaml

from .manifest import (
    _CHANGE_FILE_RE,
    BASELINE_MANIFEST_SCHEMA,
    BASELINE_STATE_FROZEN,
    BASELINE_STATE_PENDING,
    AuthorityManifestError,
    BaselineManifest,
    ReferenceDataSet,
    _validate_full_sha256,
    _validate_identifier,
    _validate_ledger_id,
    _validate_parent_id,
    _validate_relation_name,
    file_sha256,
)

_BASELINE_PROVENANCE_INPUTS = (
    "database/schema.sql",
    "database/migrations/legacy-manifest.yml",
    "database/migrations/2026_08_post_kb_v1/manifest.yml",
    "database/migrations/per_service/manifest.yml",
    "database/authority/fingerprint.py",
    "database/authority/fingerprint_catalog.py",
    "database/authority/fingerprint_values.py",
    "database/bootstrap/roles.sql",
    "database/bootstrap/extensions.sql",
    "scripts/inventory/database_policy.py",
    "scripts/inventory/generate_database_grants.py",
    "scripts/database/render_baseline_contract.py",
    "scripts/database/freeze_baseline.py",
    ":(glob)database/migrations/**/*.sql",
)
_REQUIRED_BASELINE_FILES = frozenset(
    {
        "cutover_convergence.sql",
        "grants.sql",
        "init.sql",
        "reference_data.sql",
        "verify.sql",
    }
)
_REQUIRED_POLICY_FILES = frozenset(
    {
        "data-access-inventory.json",
        "ownership-policy.json",
        "grants-policy.json",
    }
)


def _read_json(path: Path) -> dict[str, object]:
    if not path.exists():
        raise AuthorityManifestError(f"baseline manifest not found: {path}")
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise AuthorityManifestError(f"invalid JSON in {path}: {exc}") from exc
    if not isinstance(raw, dict):
        raise AuthorityManifestError(f"baseline manifest {path} must be an object")
    return raw


def _validate_header(path: Path, raw: dict[str, object]) -> str:
    required = (
        "schema",
        "state",
        "baseline_id",
        "schema_revision",
        "source_git_sha",
        "last_legacy_change",
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
        "generator",
        "generated_at",
        "postgres_version",
        "files_sha256",
        "policy_files_sha256",
    )
    missing = [key for key in required if not raw.get(key)]
    if missing:
        raise AuthorityManifestError(
            f"baseline manifest {path} missing fields: {sorted(missing)}"
        )
    if raw["schema"] != BASELINE_MANIFEST_SCHEMA:
        raise AuthorityManifestError(
            f"baseline manifest {path}: schema must be {BASELINE_MANIFEST_SCHEMA!r}"
        )
    if raw["state"] != BASELINE_STATE_FROZEN:
        raise AuthorityManifestError(
            f"baseline manifest {path}: state is {raw['state']!r}, not frozen"
        )
    baseline_id = _validate_ledger_id(raw["baseline_id"], "baseline_id")
    _validate_parent_id(path, baseline_id, "baseline")
    if not re.fullmatch(r"[0-9a-f]{40}", str(raw["source_git_sha"])):
        raise AuthorityManifestError(
            f"baseline manifest {path}: source_git_sha must be a full lowercase Git SHA"
        )
    if not _CHANGE_FILE_RE.fullmatch(str(raw["last_legacy_change"])):
        raise AuthorityManifestError(
            f"baseline manifest {path}: last_legacy_change is not a forward migration filename"
        )
    try:
        generated_at = datetime.fromisoformat(
            str(raw["generated_at"]).replace("Z", "+00:00")
        )
    except ValueError as exc:
        raise AuthorityManifestError(
            f"baseline manifest {path}: generated_at must be an ISO-8601 timestamp"
        ) from exc
    if generated_at.tzinfo is None:
        raise AuthorityManifestError(
            f"baseline manifest {path}: generated_at must include a timezone"
        )
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)*", str(raw["postgres_version"])):
        raise AuthorityManifestError(f"baseline manifest {path}: postgres_version is invalid")
    if not re.fullmatch(r"[0-9]+", str(raw["schema_revision"])):
        raise AuthorityManifestError(
            f"baseline manifest {path}: schema_revision must be numeric"
        )
    for field in (
        "structural_sha256",
        "acl_sha256",
        "extensions_sha256",
        "reference_data_sha256",
    ):
        _validate_full_sha256(raw[field], field)
    return baseline_id


def _validate_files(path: Path, raw: dict[str, object]) -> tuple[tuple[str, str], ...]:
    raw_digests = raw["files_sha256"]
    if not isinstance(raw_digests, dict):
        raise AuthorityManifestError(f"baseline manifest {path}: files_sha256 must be an object")
    declared = set(raw_digests)
    actual = {candidate.name for candidate in path.parent.glob("*.sql") if candidate.is_file()}
    if declared != _REQUIRED_BASELINE_FILES or actual != _REQUIRED_BASELINE_FILES:
        raise AuthorityManifestError(
            f"baseline manifest {path}: SQL coverage mismatch; "
            f"required={sorted(_REQUIRED_BASELINE_FILES)}, declared={sorted(declared)}, "
            f"actual={sorted(actual)}"
        )
    files: list[tuple[str, str]] = []
    for filename in sorted(_REQUIRED_BASELINE_FILES):
        digest = _validate_full_sha256(
            raw_digests[filename], f"files_sha256.{filename}"
        )
        actual_digest = file_sha256(path.parent / filename)
        if actual_digest != digest:
            raise AuthorityManifestError(
                f"baseline manifest {path}: checksum drift for {filename}: "
                f"declared {digest}, actual {actual_digest}"
            )
        files.append((filename, digest))
    return tuple(files)


def _validate_policy_files(
    path: Path, raw: dict[str, object]
) -> tuple[tuple[str, str], ...]:
    raw_digests = raw["policy_files_sha256"]
    if not isinstance(raw_digests, dict) or set(raw_digests) != _REQUIRED_POLICY_FILES:
        raise AuthorityManifestError(
            f"baseline manifest {path}: policy_files_sha256 must cover exactly "
            f"{sorted(_REQUIRED_POLICY_FILES)}"
        )
    files: list[tuple[str, str]] = []
    for filename in sorted(_REQUIRED_POLICY_FILES):
        digest = _validate_full_sha256(
            raw_digests[filename], f"policy_files_sha256.{filename}"
        )
        policy_path = path.parent / filename
        if not policy_path.is_file() or file_sha256(policy_path) != digest:
            raise AuthorityManifestError(
                f"baseline manifest {path}: checksum drift for {filename}"
            )
        files.append((filename, digest))
    return tuple(files)


def _reference_data(path: Path, raw: dict[str, object]) -> tuple[ReferenceDataSet, ...]:
    entries = raw.get("reference_data", [])
    if not isinstance(entries, list):
        raise AuthorityManifestError(f"baseline manifest {path}: reference_data must be a list")
    result: list[ReferenceDataSet] = []
    seen: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or "table" not in entry:
            raise AuthorityManifestError(
                f"baseline manifest {path}: reference_data entries must name a table"
            )
        table = _validate_relation_name(entry["table"], "reference_data.table")
        if table in seen:
            raise AuthorityManifestError(
                f"baseline manifest {path}: reference_data duplicates table {table!r}"
            )
        seen.add(table)
        natural_key = entry.get("natural_key")
        immutable = entry.get("immutable_columns")
        if not isinstance(entry.get("where", ""), str):
            raise AuthorityManifestError(
                f"baseline manifest {path}: reference_data.where must be a string"
            )
        if (
            not isinstance(natural_key, list)
            or not natural_key
            or any(not isinstance(column, str) for column in natural_key)
            or len(natural_key) != len(set(natural_key))
            or not isinstance(immutable, list)
            or not immutable
            or any(not isinstance(column, str) for column in immutable)
            or len(immutable) != len(set(immutable))
        ):
            raise AuthorityManifestError(
                f"baseline manifest {path}: reference_data entry for {table!r} "
                "needs natural_key and immutable_columns with unique column names"
            )
        where = entry.get("where", "")
        assert isinstance(where, str)
        if any(marker in where for marker in (";", "--", "/*", "*/")) or any(
            ord(character) < 32 for character in where
        ):
            raise AuthorityManifestError(
                f"baseline manifest {path}: reference_data.where must be one SQL expression"
            )
        result.append(
            ReferenceDataSet(
                table=table,
                natural_key=tuple(
                    _validate_identifier(column, "natural_key") for column in natural_key
                ),
                immutable_columns=tuple(
                    _validate_identifier(column, "immutable_columns") for column in immutable
                ),
                where=where,
            )
        )
    return tuple(result)


def load_baseline_manifest(path: Path) -> BaselineManifest:
    """Parse and validate one frozen baseline manifest.json, fail closed."""
    raw = _read_json(path)
    baseline_id = _validate_header(path, raw)
    return BaselineManifest(
        baseline_id=baseline_id,
        schema_revision=str(raw["schema_revision"]),
        source_git_sha=str(raw["source_git_sha"]),
        last_legacy_change=str(raw["last_legacy_change"]),
        structural_sha256=str(raw["structural_sha256"]),
        acl_sha256=str(raw["acl_sha256"]),
        extensions_sha256=str(raw["extensions_sha256"]),
        reference_data_sha256=str(raw["reference_data_sha256"]),
        generator=str(raw["generator"]),
        generated_at=str(raw["generated_at"]),
        postgres_version=str(raw["postgres_version"]),
        files_sha256=_validate_files(path, raw),
        policy_files_sha256=_validate_policy_files(path, raw),
        reference_data=_reference_data(path, raw),
    )


def baseline_artifact_state(path: Path) -> str:
    """Read activation state without treating pending files as frozen."""
    if not path.exists():
        return "missing"
    raw = _read_json(path)
    if raw.get("schema") != BASELINE_MANIFEST_SCHEMA:
        raise AuthorityManifestError(
            f"baseline state file {path} must use {BASELINE_MANIFEST_SCHEMA!r}"
        )
    baseline_id = _validate_ledger_id(raw.get("baseline_id", ""), "baseline_id")
    _validate_parent_id(path, baseline_id, "baseline")
    state = raw.get("state")
    if state not in {BASELINE_STATE_PENDING, BASELINE_STATE_FROZEN}:
        raise AuthorityManifestError(
            f"baseline state file {path} has unknown state {state!r}"
        )
    return str(state)


def _source_manifest_is_pending(
    result: subprocess.CompletedProcess[str],
    *,
    baseline_id: str,
) -> bool:
    if result.returncode != 0:
        return False
    try:
        raw = json.loads(result.stdout)
    except json.JSONDecodeError:
        return False
    return (
        isinstance(raw, dict)
        and raw.get("schema") == BASELINE_MANIFEST_SCHEMA
        and raw.get("state") == BASELINE_STATE_PENDING
        and raw.get("baseline_id") == baseline_id
    )


def verify_baseline_git_provenance(
    path: Path,
    baseline: BaselineManifest,
    *,
    repo_root: Path,
) -> None:
    """Prove a frozen artifact derives from an immutable pending source commit."""
    generator = PurePosixPath(baseline.generator)
    if (
        generator.is_absolute()
        or not baseline.generator
        or generator.as_posix() != baseline.generator
        or ".." in generator.parts
    ):
        raise AuthorityManifestError(
            f"baseline manifest {path}: generator must be a safe repository-relative path"
        )
    try:
        manifest_rel = path.resolve().relative_to(repo_root.resolve()).as_posix()
        cutover_rel = (path.parent / "cutover_convergence.sql").resolve().relative_to(
            repo_root.resolve()
        ).as_posix()
    except ValueError as exc:
        raise AuthorityManifestError(
            f"baseline manifest {path} is outside repository {repo_root}"
        ) from exc

    def git(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=repo_root,
            capture_output=True,
            text=True,
            check=False,
        )

    source = baseline.source_git_sha
    resolved = git("rev-parse", "--verify", f"{source}^{{commit}}")
    if resolved.returncode != 0 or resolved.stdout.strip() != source:
        raise AuthorityManifestError(
            f"baseline manifest {path}: source_git_sha is not a resolvable exact commit"
        )
    if git("merge-base", "--is-ancestor", source, "HEAD").returncode != 0:
        raise AuthorityManifestError(
            f"baseline manifest {path}: source_git_sha is not an ancestor of HEAD"
        )
    source_manifest = git("show", f"{source}:{manifest_rel}")
    if not _source_manifest_is_pending(source_manifest, baseline_id=baseline.baseline_id):
        raise AuthorityManifestError(
            f"baseline manifest {path}: source commit must contain the matching "
            "pending-live-freeze manifest, never a prior frozen artifact"
        )
    if git("cat-file", "-e", f"{source}:{generator.as_posix()}").returncode != 0:
        raise AuthorityManifestError(
            f"baseline manifest {path}: generator is absent from source_git_sha"
        )
    inputs = [
        *_BASELINE_PROVENANCE_INPUTS,
        generator.as_posix(),
        cutover_rel,
        # Epochs extend the frozen database; they are not baseline generator
        # inputs after freeze. Their own immutable history is checked below.
        f":(exclude)database/migrations/{baseline.baseline_id}/**",
    ]
    if git("diff", "--quiet", source, "--", generator.as_posix(), cutover_rel).returncode != 0:
        raise AuthorityManifestError(f"baseline manifest {path}: authority inputs differ from source_git_sha")
    comparison = git("diff", "--quiet", source, "--", *inputs)
    if comparison.returncode == 1:
        raise AuthorityManifestError(
            f"baseline manifest {path}: authority inputs differ from source_git_sha"
        )
    if comparison.returncode != 0:
        detail = (comparison.stderr or comparison.stdout).strip()
        raise AuthorityManifestError(
            f"baseline manifest {path}: cannot verify source provenance: {detail}"
        )
    _verify_epoch_additions(baseline.baseline_id, source, repo_root, git)


def _verify_epoch_additions(baseline_id: str, source: str, repo_root: Path, git) -> None:
    """Freeze each integrated epoch prefix while allowing checked additions.

    The freeze source may have an empty epoch. Therefore checking only that
    source is insufficient: integrated first-parent history and the working
    manifest must retain every earlier descriptor exactly. The manifest loader
    independently binds every current SQL file to its immutable checksum.
    """
    relative = f"database/migrations/{baseline_id}/manifest.yml"
    current = repo_root / relative
    initial = git("show", f"{source}:{relative}")
    if initial.returncode != 0:
        tree = git("ls-tree", source, "--", relative)
        if tree.returncode != 0 or tree.stdout.strip():
            raise AuthorityManifestError("cannot read the source epoch manifest")
    revisions = git("log", "--first-parent", "--reverse", "--format=%H", f"{source}..HEAD", "--", relative)
    if revisions.returncode != 0:
        raise AuthorityManifestError("cannot verify integrated epoch history")
    if initial.returncode != 0 and not current.exists():
        if not revisions.stdout.strip():
            return
        raise AuthorityManifestError("an integrated epoch manifest was removed")
    from .manifest import load_epoch_manifest

    load_epoch_manifest(current)

    def descriptors(text: str) -> list[object]:
        try:
            raw = yaml.safe_load(text)
        except yaml.YAMLError as exc:
            raise AuthorityManifestError("epoch provenance contains invalid YAML") from exc
        if not isinstance(raw, dict) or raw.get("baseline_id") != baseline_id or not isinstance(raw.get("changes"), list):
            raise AuthorityManifestError("epoch provenance has an invalid baseline or changes")
        return raw["changes"]

    previous = descriptors(initial.stdout) if initial.returncode == 0 else []
    candidates = []
    for revision in revisions.stdout.splitlines():
        recorded = git("show", f"{revision}:{relative}")
        if recorded.returncode != 0:
            raise AuthorityManifestError("an integrated epoch manifest was removed")
        candidates.append(recorded.stdout)
    candidates.append(current.read_text(encoding="utf-8"))
    for candidate in candidates:
        changes = descriptors(candidate)
        if changes[:len(previous)] != previous or len(changes) < len(previous):
            raise AuthorityManifestError("integrated epoch changes are immutable; only additions are allowed")
        previous = changes
