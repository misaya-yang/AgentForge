"""Immutable carrier for the historical per-service migration track."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

import yaml

from .manifest import (
    AuthorityManifestError,
    RollbackClass,
    TransactionMode,
    file_sha256,
)

PER_SERVICE_MANIFEST_SCHEMA = "migration-authority/per-service-manifest/v1"
PER_SERVICE_MANIFEST_NAME = "manifest.yml"
PER_SERVICE_ORDER = ("_global", "gateway", "assistant", "knowledge")
HISTORICAL_MARKER_NOTES = {
    "phase6_schemas_created": "gateway/assistant/knowledge schemas + search_path",
    "phase6_tables_moved": "tables relocated from public to gateway/assistant/knowledge",
}
_FILE_RE = re.compile(
    r"^(?P<service>_global|gateway|assistant|knowledge)/"
    r"(?P<filename>[0-9]{3}_[a-z0-9_]+\.sql)$"
)
_MARKER_RE = re.compile(r"^[a-z][a-z0-9_]{0,127}$")
_FULL_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


@dataclass(frozen=True)
class PerServiceChangeSpec:
    key: str
    file: str
    sha256: str
    transaction_mode: TransactionMode
    rollback_class: RollbackClass
    historical_markers: tuple[str, ...] = ()

    def is_recorded(self, applied: set[str]) -> bool:
        return self.key in applied or bool(set(self.historical_markers) & applied)


@dataclass(frozen=True)
class PerServiceManifest:
    schema: str
    changes: tuple[PerServiceChangeSpec, ...]


def _enum(enum_type: type, value: object, context: str):
    try:
        return enum_type(value)
    except (TypeError, ValueError) as exc:
        raise AuthorityManifestError(f"{context}: invalid {enum_type.__name__} {value!r}") from exc


def _sort_key(file: str) -> tuple[int, str]:
    service, filename = file.split("/", 1)
    return PER_SERVICE_ORDER.index(service), filename


def load_per_service_manifest(path: Path) -> PerServiceManifest:
    """Load exact per-service SQL identities before any database write."""
    if not path.exists():
        raise AuthorityManifestError(f"per-service manifest not found: {path}")
    try:
        raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise AuthorityManifestError(f"invalid YAML in {path}: {exc}") from exc
    if not isinstance(raw, dict) or set(raw) != {"schema", "changes"}:
        raise AuthorityManifestError(
            f"per-service manifest {path} fields must be exactly ['changes', 'schema']"
        )
    if raw["schema"] != PER_SERVICE_MANIFEST_SCHEMA:
        raise AuthorityManifestError(
            f"per-service manifest {path} schema must be {PER_SERVICE_MANIFEST_SCHEMA!r}"
        )
    raw_changes = raw["changes"]
    if not isinstance(raw_changes, list) or not raw_changes:
        raise AuthorityManifestError(
            f"per-service manifest {path}: changes must be a non-empty list"
        )

    required = {
        "key",
        "file",
        "sha256",
        "transaction_mode",
        "rollback_class",
        "historical_markers",
    }
    changes: list[PerServiceChangeSpec] = []
    seen_files: set[str] = set()
    seen_keys: set[str] = set()
    seen_markers: set[str] = set()
    for index, entry in enumerate(raw_changes):
        context = f"per-service manifest {path}: changes[{index}]"
        if not isinstance(entry, dict) or set(entry) != required:
            raise AuthorityManifestError(f"{context} fields must be exactly {sorted(required)}")
        file = str(entry["file"])
        match = _FILE_RE.fullmatch(file)
        if match is None:
            raise AuthorityManifestError(f"{context} has unsafe file {file!r}")
        key = str(entry["key"])
        expected_key = f"{match.group('service')}:{match.group('filename')}"
        if key != expected_key:
            raise AuthorityManifestError(f"{context} key {key!r} must equal {expected_key!r}")
        if file in seen_files or key in seen_keys:
            raise AuthorityManifestError(f"{context} duplicates {file!r}")
        seen_files.add(file)
        seen_keys.add(key)
        sha256 = str(entry["sha256"])
        if _FULL_SHA256_RE.fullmatch(sha256) is None:
            raise AuthorityManifestError(f"{context}.sha256 must be a full lowercase SHA-256")
        mode = _enum(TransactionMode, entry["transaction_mode"], context)
        if mode is not TransactionMode.TRANSACTIONAL:
            raise AuthorityManifestError(
                f"{context} is non-transactional but has no attempts/recovery ledger"
            )
        rollback = _enum(RollbackClass, entry["rollback_class"], context)
        markers = entry["historical_markers"]
        if not isinstance(markers, list) or any(
            not isinstance(marker, str) or _MARKER_RE.fullmatch(marker) is None
            for marker in markers
        ):
            raise AuthorityManifestError(
                f"{context}.historical_markers must be safe ledger strings"
            )
        if len(markers) != len(set(markers)):
            raise AuthorityManifestError(f"{context}.historical_markers contains duplicates")
        repeated_markers = sorted(set(markers) & seen_markers)
        if repeated_markers:
            raise AuthorityManifestError(f"{context}.historical_markers reuse {repeated_markers}")
        seen_markers.update(markers)
        changes.append(
            PerServiceChangeSpec(
                key=key,
                file=file,
                sha256=sha256,
                transaction_mode=mode,
                rollback_class=rollback,
                historical_markers=tuple(markers),
            )
        )

    declared_order = [change.file for change in changes]
    expected_order = sorted(declared_order, key=_sort_key)
    if declared_order != expected_order:
        raise AuthorityManifestError(
            f"per-service manifest {path}: changes are not in deterministic service/file order"
        )
    actual_files = {
        candidate.relative_to(path.parent).as_posix()
        for candidate in path.parent.rglob("*.sql")
        if candidate.is_file()
    }
    if actual_files != seen_files:
        raise AuthorityManifestError(
            f"per-service manifest {path} SQL coverage mismatch; "
            f"undeclared={sorted(actual_files - seen_files)}, "
            f"missing={sorted(seen_files - actual_files)}"
        )
    for change in changes:
        actual_sha = file_sha256(path.parent / change.file)
        if actual_sha != change.sha256:
            raise AuthorityManifestError(
                f"per-service manifest {path}: checksum drift for {change.file}: "
                f"declared {change.sha256}, actual {actual_sha}"
            )
    return PerServiceManifest(PER_SERVICE_MANIFEST_SCHEMA, tuple(changes))
