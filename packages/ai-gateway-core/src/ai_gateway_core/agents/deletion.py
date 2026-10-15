"""Canonical Agent runtime-memory cleanup receipts.

The Gateway persists an immutable cleanup plan and Assistant-produced source
inventory before any cross-store deletion is attempted.  Both services use
these helpers so receipt digests cannot drift between implementations.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Mapping
from datetime import datetime
from typing import Any, Final, cast

RUNTIME_CLEANUP_PLAN_SCHEMA: Final = "agent-runtime-cleanup-plan/v1"
RUNTIME_CLEANUP_INVENTORY_SCHEMA: Final = "agent-runtime-cleanup-inventory/v1"
RUNTIME_CLEANUP_RECEIPT_SCHEMA: Final = "agent-runtime-cleanup-receipt/v1"

_MEMORY_PRINCIPAL_RE: Final = re.compile(r"^(?:am_[0-9a-f]{61}|agent-memory:[0-9a-f]{64})$")
_SOURCE_HANDLE_RE: Final = re.compile(r"^memsrc_[0-9a-f]{32}$")
_SOURCE_SCOPE_HANDLE_RE: Final = re.compile(r"^memscope_[0-9a-f]{32}$")
_VECTOR_COLLECTION_HANDLE_RE: Final = re.compile(r"^memvec_[0-9a-f]{32}$")
_DIGEST_RE: Final = re.compile(r"^sha256:[0-9a-f]{64}$")
_STABLE_CODE_RE: Final = re.compile(r"^[a-z][a-z0-9_]{0,127}$")
_POINT_ID_RE: Final = re.compile(r"^[A-Za-z0-9_.:-]{1,255}$")


def _is_nonnegative_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _has_exact_keys(value: Mapping[str, Any], expected: set[str]) -> bool:
    return set(value) == expected


def canonical_cleanup_digest(value: Mapping[str, Any]) -> str:
    """Return a stable digest after excluding self-referential digest fields."""

    canonical = {
        key: child
        for key, child in value.items()
        if key not in {"plan_digest", "inventory_digest", "receipt_digest"}
    }
    encoded = json.dumps(
        canonical,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return f"sha256:{hashlib.sha256(encoded).hexdigest()}"


def is_memory_principal_handle(value: object) -> bool:
    return bool(_MEMORY_PRINCIPAL_RE.fullmatch(str(value or "")))


def is_memory_source_handle(value: object) -> bool:
    return bool(_SOURCE_HANDLE_RE.fullmatch(str(value or "")))


def is_memory_source_scope_handle(value: object) -> bool:
    return bool(_SOURCE_SCOPE_HANDLE_RE.fullmatch(str(value or "")))


def is_cleanup_digest(value: object) -> bool:
    return bool(_DIGEST_RE.fullmatch(str(value or "")))


def build_runtime_cleanup_plan(
    *,
    deletion_id: str,
    tenant_id: str,
    agent_id: str,
    scope: str,
    subject_user_id: str | None,
    cutoff_at: str,
    principal_handles: list[str],
) -> dict[str, Any]:
    """Build an immutable, host-path-free cleanup authority plan."""

    principals = sorted({str(item) for item in principal_handles})
    if any(not is_memory_principal_handle(item) for item in principals):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PRINCIPAL_INVALID")
    plan: dict[str, Any] = {
        "schema_version": RUNTIME_CLEANUP_PLAN_SCHEMA,
        "deletion_id": str(deletion_id),
        "tenant_id": str(tenant_id),
        "agent_id": str(agent_id),
        "scope": str(scope),
        "subject_user_id": str(subject_user_id) if subject_user_id else None,
        "cutoff_at": str(cutoff_at),
        "principal_handles": principals,
        "principal_count": len(principals),
    }
    plan["plan_digest"] = canonical_cleanup_digest(plan)
    return plan


def validate_runtime_cleanup_plan(value: object) -> dict[str, Any]:
    """Validate and copy a frozen cleanup plan."""

    if not isinstance(value, Mapping):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    plan = dict(value)
    if plan.get("schema_version") != RUNTIME_CLEANUP_PLAN_SCHEMA:
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    if not _has_exact_keys(
        plan,
        {
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "scope",
            "subject_user_id",
            "cutoff_at",
            "principal_handles",
            "principal_count",
            "plan_digest",
        },
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    principals = plan.get("principal_handles")
    if not isinstance(principals, list) or any(
        not is_memory_principal_handle(item) for item in principals
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    if principals != sorted(set(principals)):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    if int(plan.get("principal_count", -1)) != len(principals):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    digest = plan.get("plan_digest")
    if not is_cleanup_digest(digest) or digest != canonical_cleanup_digest(plan):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_DIGEST_INVALID")
    for key in ("deletion_id", "tenant_id", "agent_id", "scope", "cutoff_at"):
        if not str(plan.get(key) or "").strip():
            raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    try:
        cutoff_at = datetime.fromisoformat(str(plan["cutoff_at"]).replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID") from exc
    if cutoff_at.tzinfo is None:
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    scope = str(plan["scope"])
    if scope not in {"retention", "user", "tenant"} or (
        (scope == "user") != bool(plan.get("subject_user_id"))
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_PLAN_INVALID")
    return plan


def validate_runtime_cleanup_inventory(
    value: object,
    *,
    plan: Mapping[str, Any],
) -> dict[str, Any]:
    """Validate Assistant's frozen source inventory against a cleanup plan."""

    frozen_plan = validate_runtime_cleanup_plan(plan)
    if not isinstance(value, Mapping):
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
    inventory = dict(value)
    if inventory.get("schema_version") != RUNTIME_CLEANUP_INVENTORY_SCHEMA:
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
    if not _has_exact_keys(
        inventory,
        {
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "plan_digest",
            "cutoff_at",
            "principal_count",
            "source_count",
            "vector_count",
            "principals",
            "inventory_digest",
        },
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
    for key in ("deletion_id", "tenant_id", "agent_id", "plan_digest", "cutoff_at"):
        expected = frozen_plan[key]
        if inventory.get(key) != expected:
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_SCOPE_MISMATCH")
    principals = inventory.get("principals")
    if not isinstance(principals, list):
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
    expected_principals = set(frozen_plan["principal_handles"])
    observed_principals: set[str] = set()
    source_count = 0
    vector_count_total = 0
    for principal in principals:
        if not isinstance(principal, Mapping):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        if not _has_exact_keys(
            principal,
            {
                "principal_id",
                "source_count",
                "sources",
                "vector_count",
                "vector_sets",
            },
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        principal_id = str(principal.get("principal_id") or "")
        if principal_id in observed_principals or principal_id not in expected_principals:
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_SCOPE_MISMATCH")
        observed_principals.add(principal_id)
        sources = principal.get("sources")
        if not isinstance(sources, list):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        handles: set[str] = set()
        scope_handles: set[str] = set()
        for source in sources:
            if not isinstance(source, Mapping):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            if not _has_exact_keys(
                source,
                {
                    "logical_source_scope_handle",
                    "source_handle",
                    "version_digest",
                    "source_type",
                },
            ):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            handle = str(source.get("source_handle") or "")
            scope_handle = str(source.get("logical_source_scope_handle") or "")
            version_digest = source.get("version_digest")
            if (
                handle in handles
                or scope_handle in scope_handles
                or not is_memory_source_handle(handle)
                or not is_memory_source_scope_handle(scope_handle)
                or not is_cleanup_digest(version_digest)
                or not _STABLE_CODE_RE.fullmatch(str(source.get("source_type") or ""))
            ):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            handles.add(handle)
            scope_handles.add(scope_handle)
        if not _is_nonnegative_int(principal.get("source_count")) or principal.get(
            "source_count"
        ) != len(sources):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        source_count += len(sources)
        vector_sets = principal.get("vector_sets")
        if not isinstance(vector_sets, list):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        collection_handles: set[str] = set()
        vector_count = 0
        for vector_set in vector_sets:
            if not isinstance(vector_set, Mapping):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            if not _has_exact_keys(
                vector_set,
                {"collection_kind", "collection_handle", "point_count", "points"},
            ):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            collection_kind = str(vector_set.get("collection_kind") or "")
            collection_handle = str(vector_set.get("collection_handle") or "")
            if (
                collection_kind not in {"current", "legacy"}
                or not _VECTOR_COLLECTION_HANDLE_RE.fullmatch(collection_handle)
                or collection_handle in collection_handles
            ):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            collection_handles.add(collection_handle)
            points = vector_set.get("points")
            if not isinstance(points, list):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            point_ids: set[str] = set()
            for point in points:
                if not isinstance(point, Mapping):
                    raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
                if not _has_exact_keys(point, {"point_id", "version_digest"}):
                    raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
                point_id = str(point.get("point_id") or "")
                if (
                    not _POINT_ID_RE.fullmatch(point_id)
                    or point_id in point_ids
                    or not is_cleanup_digest(point.get("version_digest"))
                ):
                    raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
                point_ids.add(point_id)
            if not _is_nonnegative_int(vector_set.get("point_count")) or vector_set.get(
                "point_count"
            ) != len(points):
                raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
            vector_count += len(points)
        if (
            not _is_nonnegative_int(principal.get("vector_count"))
            or principal.get("vector_count") != vector_count
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
        vector_count_total += vector_count
    if observed_principals != expected_principals:
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_SCOPE_MISMATCH")
    if (
        not _is_nonnegative_int(inventory.get("principal_count"))
        or not _is_nonnegative_int(inventory.get("source_count"))
        or not _is_nonnegative_int(inventory.get("vector_count"))
        or inventory.get("principal_count") != len(principals)
        or inventory.get("source_count") != source_count
        or inventory.get("vector_count") != vector_count_total
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID")
    digest = inventory.get("inventory_digest")
    if not is_cleanup_digest(digest) or digest != canonical_cleanup_digest(inventory):
        raise ValueError("AGENT_RUNTIME_CLEANUP_INVENTORY_DIGEST_INVALID")
    return inventory


def validate_runtime_cleanup_receipt(
    value: object,
    *,
    plan: Mapping[str, Any],
    inventory: Mapping[str, Any],
) -> dict[str, Any]:
    """Validate a completed/partial execution receipt and its full lineage."""

    frozen_plan = validate_runtime_cleanup_plan(plan)
    frozen_inventory = validate_runtime_cleanup_inventory(inventory, plan=frozen_plan)
    if not isinstance(value, Mapping):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
    receipt = dict(value)
    if receipt.get("schema_version") != RUNTIME_CLEANUP_RECEIPT_SCHEMA:
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
    if not _has_exact_keys(
        receipt,
        {
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "plan_digest",
            "inventory_digest",
            "status",
            "completed",
            "retryable",
            "principals",
            "errors",
            "receipt_digest",
        },
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
    expected_fields = {
        "deletion_id": frozen_plan["deletion_id"],
        "tenant_id": frozen_plan["tenant_id"],
        "agent_id": frozen_plan["agent_id"],
        "plan_digest": frozen_plan["plan_digest"],
        "inventory_digest": frozen_inventory["inventory_digest"],
    }
    if any(receipt.get(key) != expected for key, expected in expected_fields.items()):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_SCOPE_MISMATCH")
    principals = receipt.get("principals")
    if not isinstance(principals, list):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
    expected_principals = {str(item["principal_id"]) for item in frozen_inventory["principals"]}
    inventory_by_principal = {
        str(item["principal_id"]): item for item in frozen_inventory["principals"]
    }
    observed: set[str] = set()
    observed_errors: set[str] = set()
    observed_completed: list[bool] = []
    for principal in principals:
        if not isinstance(principal, Mapping):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        if not _has_exact_keys(
            principal,
            {
                "principal_id",
                "status",
                "completed",
                "retryable",
                "source_count",
                "deleted_source_count",
                "vector_count",
                "deleted_vector_count",
                "idempotent_absent_count",
                "idempotent_absent_vector_count",
                "errors",
            },
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        principal_id = str(principal.get("principal_id") or "")
        if principal_id in observed or principal_id not in expected_principals:
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_SCOPE_MISMATCH")
        observed.add(principal_id)
        status = principal.get("status")
        completed = principal.get("completed")
        retryable = principal.get("retryable")
        if status not in {"completed", "partial"}:
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        if not isinstance(completed, bool):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        if not isinstance(retryable, bool):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        if (status == "completed") != completed or retryable == completed:
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        frozen_principal = inventory_by_principal[principal_id]
        counts: dict[str, int] = {}
        for key in (
            "source_count",
            "deleted_source_count",
            "vector_count",
            "deleted_vector_count",
            "idempotent_absent_count",
            "idempotent_absent_vector_count",
        ):
            value = principal.get(key)
            if not _is_nonnegative_int(value):
                raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
            counts[key] = cast(int, value)
        if (
            counts["source_count"] != frozen_principal["source_count"]
            or counts["vector_count"] != frozen_principal["vector_count"]
            or counts["deleted_source_count"] + counts["idempotent_absent_count"]
            > counts["source_count"]
            or counts["deleted_vector_count"] + counts["idempotent_absent_vector_count"]
            > counts["vector_count"]
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        if completed and (
            counts["deleted_source_count"] + counts["idempotent_absent_count"]
            != counts["source_count"]
            or counts["deleted_vector_count"] + counts["idempotent_absent_vector_count"]
            != counts["vector_count"]
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        errors = principal.get("errors")
        if (
            not isinstance(errors, list)
            or len(errors) != len(set(errors))
            or any(not _STABLE_CODE_RE.fullmatch(str(item or "")) for item in errors)
            or (completed and errors)
            or (not completed and not errors)
        ):
            raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
        observed_errors.update(str(item) for item in errors)
        observed_completed.append(completed)
    if observed != expected_principals:
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_SCOPE_MISMATCH")
    completed = all(observed_completed)
    expected_status = "completed" if completed else "partial"
    top_errors = receipt.get("errors")
    if (
        receipt.get("status") != expected_status
        or not isinstance(receipt.get("completed"), bool)
        or not isinstance(receipt.get("retryable"), bool)
        or receipt.get("completed") != completed
        or receipt.get("retryable") != (not completed)
        or not isinstance(top_errors, list)
        or top_errors != sorted(observed_errors)
    ):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID")
    digest = receipt.get("receipt_digest")
    if not is_cleanup_digest(digest) or digest != canonical_cleanup_digest(receipt):
        raise ValueError("AGENT_RUNTIME_CLEANUP_RECEIPT_DIGEST_INVALID")
    return receipt


def cleanup_receipt_completed(receipt: Mapping[str, Any]) -> bool:
    principals = receipt.get("principals")
    return bool(
        receipt.get("status") == "completed"
        and receipt.get("completed") is True
        and receipt.get("retryable") is False
        and isinstance(principals, list)
        and all(
            isinstance(item, Mapping)
            and item.get("status") == "completed"
            and item.get("completed") is True
            and item.get("retryable") is False
            for item in principals
        )
    )


__all__ = [
    "RUNTIME_CLEANUP_INVENTORY_SCHEMA",
    "RUNTIME_CLEANUP_PLAN_SCHEMA",
    "RUNTIME_CLEANUP_RECEIPT_SCHEMA",
    "build_runtime_cleanup_plan",
    "canonical_cleanup_digest",
    "cleanup_receipt_completed",
    "is_cleanup_digest",
    "is_memory_principal_handle",
    "is_memory_source_handle",
    "is_memory_source_scope_handle",
    "validate_runtime_cleanup_inventory",
    "validate_runtime_cleanup_plan",
    "validate_runtime_cleanup_receipt",
]
