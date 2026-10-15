"""Evaluator for fail-closed historical numeric-ledger reconciliation.

Evidence SQL and immutable identity matching live in :mod:`numeric_evidence`;
this module owns the version-specific decision policy and public API.
"""

from __future__ import annotations

import re
from typing import Any

from .discovery import HISTORICAL_NUMERIC_IDENTITIES, LEGACY_FILENAME_ALIASES
from .manifest import LegacyManifest
from .numeric_evidence import (
    NUMERIC_EVIDENCE_SQL,
    NumericLedgerRecord,
    NumericReconciliationBlocked,
    NumericReconciliationReceipt,
    _match_numeric_identity,
    _numeric_evidence,
    _numeric_ledger_records,
)

__all__ = [
    "NUMERIC_EVIDENCE_SQL",
    "NumericLedgerRecord",
    "NumericReconciliationBlocked",
    "NumericReconciliationReceipt",
    "reconcile_numeric_legacy_history",
]


async def reconcile_numeric_legacy_history(
    conn: Any, legacy_manifest: LegacyManifest
) -> NumericReconciliationReceipt:
    """Prove historic 016/030/031 rows before duplicate-chain validation."""
    receipt = NumericReconciliationReceipt()
    forward = legacy_manifest.by_file()
    rollbacks = legacy_manifest.rollback_by_file()
    identities: dict[str, tuple[str, str]] = {
        filename: (spec.sha256, spec.legacy_checksum) for filename, spec in forward.items()
    }
    identities.update(
        {filename: (spec.sha256, spec.legacy_checksum) for filename, spec in rollbacks.items()}
    )

    records_by_version: dict[str, list[NumericLedgerRecord]] = {}
    for record in await _numeric_ledger_records(conn):
        records_by_version.setdefault(record.version, []).append(record)

    # Validate every row that carries evidence, not only the three ambiguous
    # historical revisions. Bare version-only ledgers remain supported for a
    # unique revision, but dirty, unknown, or contradictory identities block.
    forward_by_version: dict[str, list[str]] = {}
    for filename in forward:
        forward_by_version.setdefault(filename[:3], []).append(filename)
    for version, records in sorted(records_by_version.items()):
        if version in HISTORICAL_NUMERIC_IDENTITIES:
            continue
        entry: dict[str, Any] = {}
        receipt.versions[version] = entry
        if len(records) != 1:
            receipt.block(version, f"numeric ledger has {len(records)} rows for one version")
            continue
        record = records[0]
        entry["ledger"] = {
            "name": record.name,
            "checksum": record.checksum,
            "dirty": record.dirty,
        }
        if record.dirty:
            receipt.block(version, "numeric ledger row is dirty")
            continue
        candidates = forward_by_version.get(version, [])
        if len(candidates) != 1:
            receipt.block(
                version,
                "numeric ledger version has no unique immutable forward file",
            )
            continue
        filename = candidates[0]
        spec = forward[filename]
        accepted_names = {filename, _legacy_description(filename)}
        alias = LEGACY_FILENAME_ALIASES.get(filename)
        if alias:
            accepted_names.update({alias, _legacy_description(alias)})
        if record.name and record.name not in accepted_names:
            receipt.block(
                version,
                f"ledger name {record.name!r} does not identify {filename}",
            )
            continue
        if record.checksum:
            if not _valid_checksum_shape(record.checksum):
                receipt.block(version, f"invalid legacy checksum shape {record.checksum!r}")
                continue
            if record.checksum not in {spec.sha256, spec.legacy_checksum}:
                receipt.block(
                    version,
                    f"checksum {record.checksum!r} does not match immutable {filename}",
                )
                continue
        entry["identified_file"] = filename
        entry["identity_basis"] = (
            "unique_version_only"
            if not record.name and not record.checksum
            else "unique_version_with_matching_identity"
        )
        entry["verdict"] = "proven"

    for version, ordered_identities in HISTORICAL_NUMERIC_IDENTITIES.items():
        records = records_by_version.get(version, [])
        entry: dict[str, Any] = {"ordered_historical_identities": list(ordered_identities)}
        receipt.versions[version] = entry
        if not records:
            if version == "031":
                receipt.block(
                    version,
                    "version absent: the data-only administrator price migration cannot be "
                    "proven and will not be replayed automatically",
                )
                continue
            entry.update(
                {
                    "verdict": "not_applied",
                    "reason": "version absent; immutable forward file(s) remain pending",
                }
            )
            continue
        if len(records) != 1:
            receipt.block(version, f"numeric ledger has {len(records)} rows for one version")
            continue

        record = records[0]
        entry["ledger"] = {
            "name": record.name,
            "checksum": record.checksum,
            "dirty": record.dirty,
        }
        if record.dirty:
            receipt.block(version, "numeric ledger row is dirty")
            continue
        candidates = {
            filename: identities[filename]
            for filename in ordered_identities
            if filename in identities
        }
        identity, error, basis = _match_numeric_identity(record, candidates, ordered_identities)
        if error is not None or identity is None:
            receipt.block(version, error or "numeric identity is unprovable")
            continue
        entry["identified_file"] = identity
        entry["identity_basis"] = basis

        if version == "016":
            expected_last = ordered_identities[-1]
            if identity != expected_last:
                receipt.block(
                    version,
                    f"ledger identifies {identity}; later sibling {expected_last} is not proven",
                )
                continue
            evidence = await _numeric_evidence(conn, "016_effective")
            entry["evidence"] = [evidence]
            if not evidence["passed"]:
                receipt.block(version, "016 object/index/backfill evidence is incomplete")
                continue
            entry["verdict"] = "proven"
            entry["reason"] = "last sibling identity plus complete effective-state evidence"
            continue

        if version == "030":
            forward_evidence = await _numeric_evidence(conn, "030_forward")
            rollback_evidence = await _numeric_evidence(conn, "030_rollback")
            entry["evidence"] = [forward_evidence, rollback_evidence]
            forward_file, rollback_file = ordered_identities
            if identity == rollback_file:
                receipt.block(
                    version,
                    "historical rollback execution is proven; forward state must not be guessed or replayed",
                )
            elif identity != forward_file:
                receipt.block(version, f"unexpected 030 identity {identity}")
            elif forward_evidence["passed"] and not rollback_evidence["passed"]:
                entry["verdict"] = "proven"
                entry["reason"] = "forward identity and complete post-forward state agree"
            else:
                receipt.block(
                    version,
                    "030 schema/data evidence is rollback, mixed, or otherwise unproven",
                )
            continue

        if version == "031":
            expected_last = ordered_identities[-1]
            if identity != expected_last:
                receipt.block(
                    version,
                    "data-only price migration is not followed by a proven hierarchical sibling; "
                    "administrator prices will not be replayed",
                )
                continue
            evidence = await _numeric_evidence(conn, "031_hierarchy_effective")
            entry["evidence"] = [evidence]
            if not evidence["passed"]:
                receipt.block(
                    version, "031 hierarchy object/constraint/data evidence is incomplete"
                )
                continue
            entry["verdict"] = "proven"
            entry["reason"] = (
                "last sibling ledger identity proves the old runner passed the earlier data-only "
                "change; mutable administrator prices are intentionally neither compared nor replayed"
            )

    if receipt.verdict == "proven":
        receipt.notes.append(
            "numeric duplicate history is safe to continue; this receipt never mutates a legacy ledger"
        )
    return receipt


def _legacy_description(filename: str) -> str:
    return filename[4:-4].replace("_", " ").title()


def _valid_checksum_shape(value: str) -> bool:
    return len(value) in (16, 64) and re.fullmatch(r"[0-9a-f]+", value) is not None
