"""Manifest models for the migration authority.

Two machine carriers:

* ``database/migrations/<baseline_id>/manifest.yml`` — one epoch. Declares per
  change the full SHA-256, owner, transaction mode, rollback class, pre/post
  conditions, timeout/lock budget and resume/repair handler.  The runner and
  CI read this file only; they never infer intent from report prose.
* ``database/baselines/<baseline_id>/manifest.json`` — one frozen baseline.
  Declares baseline id, source Git SHA, last legacy change, the four
  fingerprints, generator identity and the reference-data policy.
"""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

import yaml

from .constants import DEFAULT_BASELINE_ID, LOGICAL_PRINCIPALS


class AuthorityManifestError(RuntimeError):
    """A manifest failed validation; the runner must fail closed."""


class TransactionMode(str, Enum):
    TRANSACTIONAL = "transactional"
    NON_TRANSACTIONAL = "non_transactional"


class RollbackClass(str, Enum):
    OLD_BINARY_COMPATIBLE = "old-binary-compatible"
    FORWARD_FIX_ONLY = "forward-fix-only"
    RESTORE_REQUIRED = "restore-required"


_CHANGE_FILE_RE = re.compile(r"^(\d{3})_[a-z0-9_]+\.sql$")
_LEDGER_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_]{0,62}$")
_FULL_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
BASELINE_MANIFEST_SCHEMA = "migration-authority/baseline-manifest/v1"
BASELINE_STATE_PENDING = "pending-live-freeze"
BASELINE_STATE_FROZEN = "frozen"
LEGACY_MANIFEST_SCHEMA = "migration-authority/legacy-manifest/v1"
LEGACY_NON_TRANSACTIONAL_FILES = frozenset({"049_session_list_performance.sql"})
_REQUIRED_CHANGE_FIELDS = (
    "sequence",
    "name",
    "file",
    "sha256",
    "owner",
    "transaction_mode",
    "rollback_class",
    "preconditions",
    "postconditions",
    "timeout_seconds",
    "lock_budget_seconds",
    "resume_handler",
    "repair_handler",
)


@dataclass(frozen=True)
class ChangeSpec:
    """One immutable, manifest-declared migration change."""

    sequence: int
    name: str
    file: str
    sha256: str
    owner: str
    transaction_mode: TransactionMode
    rollback_class: RollbackClass
    preconditions: tuple[str, ...] = ()
    postconditions: tuple[str, ...] = ()
    timeout_seconds: int = 300
    lock_budget_seconds: int = 30
    resume_handler: str | None = None
    repair_handler: str | None = None
    notes: str = ""


@dataclass(frozen=True)
class EpochManifest:
    baseline_id: str
    epoch: int
    changes: tuple[ChangeSpec, ...] = ()
    extra: dict = field(default_factory=dict)

    def by_sequence(self) -> dict[int, ChangeSpec]:
        return {change.sequence: change for change in self.changes}


@dataclass(frozen=True)
class LegacyChangeSpec:
    """Immutable execution metadata for one pre-baseline forward file."""

    file: str
    sha256: str
    transaction_mode: TransactionMode
    rollback_class: RollbackClass

    @property
    def legacy_checksum(self) -> str:
        """Known 16-character checksum written by the historical Python runner."""
        return self.sha256[:16]


@dataclass(frozen=True)
class LegacyRollbackSpec:
    """Identity of a rollback file once auto-discovered by the old runner."""

    file: str
    sha256: str

    @property
    def legacy_checksum(self) -> str:
        return self.sha256[:16]


@dataclass(frozen=True)
class LegacyManifest:
    schema: str
    freeze_point: str
    changes: tuple[LegacyChangeSpec, ...]
    historical_rollbacks: tuple[LegacyRollbackSpec, ...] = ()

    def by_file(self) -> dict[str, LegacyChangeSpec]:
        return {change.file: change for change in self.changes}

    def rollback_by_file(self) -> dict[str, LegacyRollbackSpec]:
        return {rollback.file: rollback for rollback in self.historical_rollbacks}


@dataclass(frozen=True)
class ReferenceDataSet:
    """One system-owned immutable table slice admitted to the exact hash."""

    table: str
    natural_key: tuple[str, ...]
    immutable_columns: tuple[str, ...]
    where: str = ""


@dataclass(frozen=True)
class BaselineManifest:
    baseline_id: str
    schema_revision: str
    source_git_sha: str
    last_legacy_change: str
    structural_sha256: str
    acl_sha256: str
    extensions_sha256: str
    reference_data_sha256: str
    generator: str
    generated_at: str
    postgres_version: str
    schema: str = BASELINE_MANIFEST_SCHEMA
    state: str = BASELINE_STATE_FROZEN
    files_sha256: tuple[tuple[str, str], ...] = ()
    policy_files_sha256: tuple[tuple[str, str], ...] = ()
    reference_data: tuple[ReferenceDataSet, ...] = ()

    @property
    def fingerprints(self) -> dict[str, str]:
        return {
            "structural": self.structural_sha256,
            "acl": self.acl_sha256,
            "extensions": self.extensions_sha256,
            "reference_data": self.reference_data_sha256,
        }


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _validate_identifier(value: str, field_name: str) -> str:
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,62}", str(value)):
        raise AuthorityManifestError(
            f"manifest field {field_name!r} is not a safe identifier: {value!r}"
        )
    return str(value)


def _validate_ledger_id(value: object, field_name: str) -> str:
    """Validate an opaque ledger key, never an SQL identifier.

    Baseline ids are persisted through bind parameters and may begin with a
    digit (for example ``2026_08_post_kb_v1``). Keeping this validator
    separate from ``_validate_identifier`` prevents callers from treating a
    ledger id as a role, schema, table or column name.
    """
    rendered = str(value)
    if not _LEDGER_ID_RE.fullmatch(rendered):
        raise AuthorityManifestError(
            f"manifest field {field_name!r} is not a safe ledger id: {value!r}"
        )
    return rendered


def _validate_full_sha256(value: object, field_name: str) -> str:
    rendered = str(value)
    if not _FULL_SHA256_RE.fullmatch(rendered):
        raise AuthorityManifestError(
            f"manifest field {field_name!r} must be a full lowercase SHA-256: {value!r}"
        )
    return rendered


def _validate_parent_id(path: Path, ledger_id: str, manifest_kind: str) -> None:
    if path.parent.name != ledger_id:
        raise AuthorityManifestError(
            f"{manifest_kind} manifest {path} declares baseline_id {ledger_id!r} "
            f"but its directory is {path.parent.name!r}"
        )


def _validate_relation_name(value: str, field_name: str) -> str:
    """A table name, optionally schema-qualified (``schema.table``).

    Reference-data tables are always addressed schema-qualified so the
    fingerprint never depends on the session search_path.
    """
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,62}(\.[a-z][a-z0-9_]{0,62})?", str(value)):
        raise AuthorityManifestError(
            f"manifest field {field_name!r} is not a safe relation name: {value!r}"
        )
    return str(value)


def _as_str_tuple(value: object, field_name: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise AuthorityManifestError(f"manifest field {field_name!r} must be a list of SQL strings")
    if any(not item.strip() for item in value):
        raise AuthorityManifestError(f"manifest field {field_name!r} cannot contain blank SQL")
    return tuple(value)


def _condition_tuple(value: object, field_name: str) -> tuple[str, ...]:
    conditions = _as_str_tuple(value, field_name)
    if not conditions:
        raise AuthorityManifestError(
            f"manifest field {field_name!r} must declare at least one read-only SELECT"
        )
    for condition in conditions:
        if not re.match(r"^\s*SELECT\b", condition, re.IGNORECASE) or any(
            marker in condition for marker in (";", "--", "/*", "*/")
        ):
            raise AuthorityManifestError(
                f"manifest field {field_name!r} must contain one read-only SELECT per item"
            )
    return conditions


def _positive_int(value: object, field_name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise AuthorityManifestError(f"manifest field {field_name!r} must be a positive integer")
    return value


def _optional_identifier(value: object, field_name: str) -> str | None:
    if value is None:
        return None
    return _validate_identifier(str(value), field_name)


def load_epoch_manifest(path: Path) -> EpochManifest:
    """Parse and validate one epoch manifest.yml, fail closed."""
    if not path.exists():
        raise AuthorityManifestError(f"epoch manifest not found: {path}")
    try:
        raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise AuthorityManifestError(f"invalid YAML in {path}: {exc}") from exc
    if not isinstance(raw, dict):
        raise AuthorityManifestError(f"epoch manifest {path} must be a mapping")

    baseline_id = _validate_ledger_id(raw.get("baseline_id", ""), "baseline_id")
    _validate_parent_id(path, baseline_id, "epoch")
    epoch = raw.get("epoch")
    if not isinstance(epoch, int) or epoch < 1:
        raise AuthorityManifestError(f"epoch manifest {path} needs an integer epoch >= 1")

    raw_changes = raw.get("changes") or []
    if not isinstance(raw_changes, list):
        raise AuthorityManifestError(f"epoch manifest {path}: changes must be a list")

    changes: list[ChangeSpec] = []
    seen_sequences: set[int] = set()
    for entry in raw_changes:
        if not isinstance(entry, dict):
            raise AuthorityManifestError(f"epoch manifest {path}: change entry must be a mapping")
        allowed_change_fields = {*_REQUIRED_CHANGE_FIELDS, "notes"}
        unknown = sorted(set(entry) - allowed_change_fields)
        if unknown:
            raise AuthorityManifestError(
                f"epoch manifest {path}: change entry has unknown fields {unknown}"
            )
        missing = [key for key in _REQUIRED_CHANGE_FIELDS if key not in entry]
        if missing:
            raise AuthorityManifestError(
                f"epoch manifest {path}: change entry missing fields {sorted(missing)}"
            )
        sequence = entry["sequence"]
        if not isinstance(sequence, int) or sequence < 1:
            raise AuthorityManifestError(f"epoch manifest {path}: sequence must be an integer >= 1")
        if sequence in seen_sequences:
            raise AuthorityManifestError(f"epoch manifest {path}: duplicate sequence {sequence}")
        seen_sequences.add(sequence)
        try:
            mode = TransactionMode(entry["transaction_mode"])
        except (TypeError, ValueError) as exc:
            raise AuthorityManifestError(
                f"epoch manifest {path}: unknown transaction_mode {entry['transaction_mode']!r}"
            ) from exc
        try:
            rollback = RollbackClass(entry["rollback_class"])
        except (TypeError, ValueError) as exc:
            raise AuthorityManifestError(
                f"epoch manifest {path}: unknown rollback_class {entry['rollback_class']!r}"
            ) from exc
        owner = entry["owner"]
        if owner not in LOGICAL_PRINCIPALS:
            raise AuthorityManifestError(
                f"epoch manifest {path}: owner {owner!r} is not a logical principal "
                f"(one of {LOGICAL_PRINCIPALS})"
            )
        sha = _validate_full_sha256(entry["sha256"], f"changes[{sequence}].sha256")
        filename = str(entry["file"])
        match = _CHANGE_FILE_RE.fullmatch(filename)
        if not match or int(match.group(1)) != sequence:
            raise AuthorityManifestError(
                f"epoch manifest {path}: file {filename!r} must be named "
                f"{sequence:03d}_<snake_case>.sql"
            )
        timeout_seconds = _positive_int(
            entry["timeout_seconds"], f"changes[{sequence}].timeout_seconds"
        )
        lock_budget_seconds = _positive_int(
            entry["lock_budget_seconds"],
            f"changes[{sequence}].lock_budget_seconds",
        )
        if lock_budget_seconds > timeout_seconds:
            raise AuthorityManifestError(
                f"epoch manifest {path}: change {sequence} lock budget cannot exceed timeout"
            )
        resume_handler = _optional_identifier(
            entry["resume_handler"], f"changes[{sequence}].resume_handler"
        )
        repair_handler = _optional_identifier(
            entry["repair_handler"], f"changes[{sequence}].repair_handler"
        )
        if mode is TransactionMode.NON_TRANSACTIONAL and not (resume_handler or repair_handler):
            raise AuthorityManifestError(
                f"epoch manifest {path}: non-transactional change {sequence} "
                "must declare a resume_handler or repair_handler"
            )
        if mode is TransactionMode.TRANSACTIONAL and (resume_handler or repair_handler):
            raise AuthorityManifestError(
                f"epoch manifest {path}: transactional change {sequence} "
                "cannot declare recovery handlers"
            )
        changes.append(
            ChangeSpec(
                sequence=sequence,
                name=_validate_identifier(entry["name"], "name"),
                file=filename,
                sha256=sha,
                owner=owner,
                transaction_mode=mode,
                rollback_class=rollback,
                preconditions=_condition_tuple(entry["preconditions"], "preconditions"),
                postconditions=_condition_tuple(entry["postconditions"], "postconditions"),
                timeout_seconds=timeout_seconds,
                lock_budget_seconds=lock_budget_seconds,
                resume_handler=resume_handler,
                repair_handler=repair_handler,
                notes=str(entry.get("notes", "")),
            )
        )

    ordered = sorted(seen_sequences)
    if ordered and ordered != list(range(1, len(ordered) + 1)):
        raise AuthorityManifestError(
            f"epoch manifest {path}: sequences must be contiguous from 1, got {ordered}"
        )

    declared_files = {change.file for change in changes}
    actual_files = {candidate.name for candidate in path.parent.glob("*.sql")}
    undeclared = sorted(actual_files - declared_files)
    missing_files = sorted(declared_files - actual_files)
    if undeclared or missing_files:
        raise AuthorityManifestError(
            f"epoch manifest {path} does not exactly cover its SQL files; "
            f"undeclared={undeclared}, missing={missing_files}"
        )
    for change in changes:
        change_path = path.parent / change.file
        actual_sha = file_sha256(change_path)
        if actual_sha != change.sha256:
            raise AuthorityManifestError(
                f"epoch manifest {path}: checksum drift for {change.file}: "
                f"declared {change.sha256}, actual {actual_sha}"
            )

    return EpochManifest(
        baseline_id=baseline_id,
        epoch=epoch,
        changes=tuple(sorted(changes, key=lambda change: change.sequence)),
        extra={
            key: value
            for key, value in raw.items()
            if key not in ("baseline_id", "epoch", "changes")
        },
    )


def _legacy_sql_files(migrations_root: Path) -> dict[str, Path]:
    """Return the flat/legacy SQL namespace, rejecting duplicate filenames."""
    candidates = [migrations_root]
    legacy_dir = migrations_root / "legacy"
    if legacy_dir.is_dir():
        candidates.append(legacy_dir)

    files: dict[str, Path] = {}
    for directory in candidates:
        for candidate in sorted(directory.glob("*.sql")):
            previous = files.get(candidate.name)
            if previous is not None and previous != candidate:
                raise AuthorityManifestError(
                    f"legacy SQL {candidate.name} exists in both {previous.parent} "
                    f"and {candidate.parent}"
                )
            files[candidate.name] = candidate
    return files


def _parse_transaction_mode(value: object, context: str) -> TransactionMode:
    try:
        return TransactionMode(value)
    except (TypeError, ValueError) as exc:
        raise AuthorityManifestError(f"{context}: unknown transaction_mode {value!r}") from exc


def _parse_rollback_class(value: object, context: str) -> RollbackClass:
    try:
        return RollbackClass(value)
    except (TypeError, ValueError) as exc:
        raise AuthorityManifestError(f"{context}: unknown rollback_class {value!r}") from exc


def load_legacy_manifest(path: Path) -> LegacyManifest:
    """Load and verify the immutable pre-baseline SQL inventory.

    The manifest must cover every top-level forward and rollback SQL file in
    ``database/migrations`` (or its future ``legacy/`` directory) exactly once.
    File contents are hashed while loading, so discovery cannot proceed after
    an unreviewed addition or checksum change.
    """
    if not path.exists():
        raise AuthorityManifestError(f"legacy manifest not found: {path}")
    try:
        raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        raise AuthorityManifestError(f"invalid YAML in {path}: {exc}") from exc
    if not isinstance(raw, dict):
        raise AuthorityManifestError(f"legacy manifest {path} must be a mapping")

    allowed_top_level = {"schema", "freeze_point", "changes", "historical_rollbacks"}
    unknown_top_level = sorted(set(raw) - allowed_top_level)
    if unknown_top_level:
        raise AuthorityManifestError(
            f"legacy manifest {path} has unknown fields: {unknown_top_level}"
        )
    if raw.get("schema") != LEGACY_MANIFEST_SCHEMA:
        raise AuthorityManifestError(
            f"legacy manifest {path} schema must be {LEGACY_MANIFEST_SCHEMA!r}"
        )

    raw_changes = raw.get("changes")
    if not isinstance(raw_changes, list) or not raw_changes:
        raise AuthorityManifestError(f"legacy manifest {path}: changes must be a non-empty list")

    changes: list[LegacyChangeSpec] = []
    seen_files: set[str] = set()
    required_change_fields = {"file", "sha256", "transaction_mode", "rollback_class"}
    for index, entry in enumerate(raw_changes):
        context = f"legacy manifest {path}: changes[{index}]"
        if not isinstance(entry, dict) or set(entry) != required_change_fields:
            raise AuthorityManifestError(
                f"{context} fields must be exactly {sorted(required_change_fields)}"
            )
        filename = str(entry["file"])
        if not _CHANGE_FILE_RE.fullmatch(filename) or filename.endswith("_rollback.sql"):
            raise AuthorityManifestError(f"{context} has invalid forward filename {filename!r}")
        if filename in seen_files:
            raise AuthorityManifestError(f"{context} duplicates {filename}")
        seen_files.add(filename)
        changes.append(
            LegacyChangeSpec(
                file=filename,
                sha256=_validate_full_sha256(entry["sha256"], f"changes[{index}].sha256"),
                transaction_mode=_parse_transaction_mode(entry["transaction_mode"], context),
                rollback_class=_parse_rollback_class(entry["rollback_class"], context),
            )
        )

    raw_rollbacks = raw.get("historical_rollbacks") or []
    if not isinstance(raw_rollbacks, list):
        raise AuthorityManifestError(f"legacy manifest {path}: historical_rollbacks must be a list")
    rollbacks: list[LegacyRollbackSpec] = []
    seen_rollbacks: set[str] = set()
    for index, entry in enumerate(raw_rollbacks):
        context = f"legacy manifest {path}: historical_rollbacks[{index}]"
        if not isinstance(entry, dict) or set(entry) != {"file", "sha256"}:
            raise AuthorityManifestError(f"{context} fields must be exactly ['file', 'sha256']")
        filename = str(entry["file"])
        if not _CHANGE_FILE_RE.fullmatch(filename) or not filename.endswith("_rollback.sql"):
            raise AuthorityManifestError(f"{context} has invalid rollback filename {filename!r}")
        if filename in seen_rollbacks:
            raise AuthorityManifestError(f"{context} duplicates {filename}")
        seen_rollbacks.add(filename)
        rollbacks.append(
            LegacyRollbackSpec(
                file=filename,
                sha256=_validate_full_sha256(
                    entry["sha256"], f"historical_rollbacks[{index}].sha256"
                ),
            )
        )

    sql_files = _legacy_sql_files(path.parent)
    declared_files = seen_files | seen_rollbacks
    actual_files = set(sql_files)
    undeclared = sorted(actual_files - declared_files)
    missing = sorted(declared_files - actual_files)
    if undeclared or missing:
        raise AuthorityManifestError(
            f"legacy manifest {path} does not exactly cover its SQL files; "
            f"undeclared={undeclared}, missing={missing}"
        )

    for spec in [*changes, *rollbacks]:
        actual_sha = file_sha256(sql_files[spec.file])
        if actual_sha != spec.sha256:
            raise AuthorityManifestError(
                f"legacy manifest {path}: checksum drift for {spec.file}: "
                f"declared {spec.sha256}, actual {actual_sha}"
            )

    expected_order = sorted(
        (change.file for change in changes), key=lambda filename: (int(filename[:3]), filename)
    )
    declared_order = [change.file for change in changes]
    if declared_order != expected_order:
        raise AuthorityManifestError(
            f"legacy manifest {path}: changes are not in deterministic migration order"
        )

    freeze_point = str(raw.get("freeze_point") or "")
    if freeze_point != expected_order[-1]:
        raise AuthorityManifestError(
            f"legacy manifest {path}: freeze_point {freeze_point!r} does not match "
            f"last forward file {expected_order[-1]!r}"
        )

    non_transactional = {
        change.file
        for change in changes
        if change.transaction_mode is TransactionMode.NON_TRANSACTIONAL
    }
    if non_transactional != LEGACY_NON_TRANSACTIONAL_FILES:
        raise AuthorityManifestError(
            f"legacy manifest {path}: non-transactional files must be exactly "
            f"{sorted(LEGACY_NON_TRANSACTIONAL_FILES)}, got {sorted(non_transactional)}"
        )

    return LegacyManifest(
        schema=LEGACY_MANIFEST_SCHEMA,
        freeze_point=freeze_point,
        changes=tuple(changes),
        historical_rollbacks=tuple(rollbacks),
    )


def default_baseline_dir(database_dir: Path, baseline_id: str = DEFAULT_BASELINE_ID) -> Path:
    return database_dir / "baselines" / baseline_id


# Keep the long-standing import seam while isolating baseline artifact parsing
# and Git provenance from epoch/legacy manifest discovery.
from .baseline_manifest import (  # noqa: E402, F401
    baseline_artifact_state,
    load_baseline_manifest,
    verify_baseline_git_provenance,
)
