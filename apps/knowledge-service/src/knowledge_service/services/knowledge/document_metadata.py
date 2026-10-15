"""Typed document metadata registry and merge-patch service."""

from __future__ import annotations

import json
import math
import re
from collections.abc import Iterable, Mapping
from datetime import datetime, timezone
from typing import Any

from ...core.auth.user_resolver import UserContext
from ...core.exceptions import ValidationFailedError
from ...persistence.database import SOURCE_OWNED_DOCUMENT_METADATA_KEYS
from ...persistence.document_metadata import (
    DocumentMetadataStore,
    MetadataRegistryRevisionConflict,
)

METADATA_FIELD_NAME_RE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")
METADATA_FIELD_TYPES = frozenset({"string", "number", "datetime"})
MAX_METADATA_FIELDS = 32
MAX_METADATA_STRING_LENGTH = 2048
MAX_USER_METADATA_BYTES = 16 * 1024
RESERVED_METADATA_FIELD_NAMES = frozenset(
    {
        *SOURCE_OWNED_DOCUMENT_METADATA_KEYS,
        "tenant_id",
        "dataset_id",
        "document_id",
        "segment_id",
        "status",
        "enabled",
        "content_revision",
        "source_type",
        "language",
    }
)


def _json_size(value: Any) -> int:
    try:
        rendered = json.dumps(
            value,
            ensure_ascii=False,
            allow_nan=False,
            separators=(",", ":"),
        )
    except (TypeError, ValueError) as exc:
        raise ValidationFailedError("metadata must contain JSON values") from exc
    return len(rendered.encode("utf-8"))


def validate_registry_fields(
    fields: Iterable[Mapping[str, Any]],
    *,
    previous_fields: Iterable[Mapping[str, Any]] = (),
) -> list[dict[str, Any]]:
    normalized: list[dict[str, Any]] = []
    seen: set[str] = set()
    previous_types = {
        str(field.get("name") or ""): str(field.get("type") or "") for field in previous_fields
    }
    for raw in fields:
        name = str(raw.get("name") or "").strip()
        label = str(raw.get("label") or name).strip()
        field_type = str(raw.get("type") or "").strip().lower()
        description = str(raw.get("description") or "").strip()
        if not METADATA_FIELD_NAME_RE.fullmatch(name):
            raise ValidationFailedError(
                "metadata field names must use lower_snake_case and be at most 64 characters"
            )
        if name in RESERVED_METADATA_FIELD_NAMES:
            raise ValidationFailedError(f"metadata field '{name}' is reserved")
        if name in seen:
            raise ValidationFailedError(f"metadata field '{name}' is duplicated")
        if field_type not in METADATA_FIELD_TYPES:
            raise ValidationFailedError(f"metadata field '{name}' has an unsupported type")
        if name in previous_types and previous_types[name] != field_type:
            raise ValidationFailedError(
                f"metadata field '{name}' type is immutable; create a new field"
            )
        if not label or len(label) > 128 or len(description) > 512:
            raise ValidationFailedError("metadata field label or description is too long")
        seen.add(name)
        normalized.append(
            {
                "name": name,
                "label": label,
                "type": field_type,
                **({"description": description} if description else {}),
            }
        )
    if len(normalized) > MAX_METADATA_FIELDS:
        raise ValidationFailedError(
            f"metadata registry supports at most {MAX_METADATA_FIELDS} fields"
        )
    if _json_size(normalized) > MAX_USER_METADATA_BYTES:
        raise ValidationFailedError("metadata registry exceeds 16 KiB")
    return normalized


def _normalize_datetime(value: Any, name: str) -> str:
    if not isinstance(value, str):
        raise ValidationFailedError(f"metadata field '{name}' must be an RFC3339 datetime")
    raw = value.strip()
    try:
        parsed = datetime.fromisoformat(raw[:-1] + "+00:00" if raw.endswith("Z") else raw)
    except ValueError as exc:
        raise ValidationFailedError(f"metadata field '{name}' must be an RFC3339 datetime") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ValidationFailedError(f"metadata field '{name}' datetime must include a timezone")
    return parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def normalize_metadata_patch(
    registry: Mapping[str, Any],
    metadata_patch: Mapping[str, Any],
    metadata_remove: Iterable[str],
) -> tuple[dict[str, Any], list[str]]:
    definitions = {
        str(field.get("name")): str(field.get("type"))
        for field in registry.get("fields", [])
        if isinstance(field, Mapping)
    }
    remove = list(dict.fromkeys(str(name or "").strip() for name in metadata_remove))
    if any(not name for name in remove):
        raise ValidationFailedError("metadata_remove contains an empty field")
    overlap = set(metadata_patch).intersection(remove)
    if overlap:
        raise ValidationFailedError(
            "metadata_patch and metadata_remove overlap: " + ", ".join(sorted(overlap))
        )
    unknown = (set(metadata_patch) | set(remove)) - set(definitions)
    if unknown:
        raise ValidationFailedError("unknown metadata fields: " + ", ".join(sorted(unknown)))

    normalized: dict[str, Any] = {}
    for name, value in metadata_patch.items():
        field_type = definitions[name]
        if field_type == "string":
            if not isinstance(value, str) or len(value) > MAX_METADATA_STRING_LENGTH:
                raise ValidationFailedError(
                    f"metadata field '{name}' must be a string up to "
                    f"{MAX_METADATA_STRING_LENGTH} characters"
                )
            normalized[name] = value
        elif field_type == "number":
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ValidationFailedError(f"metadata field '{name}' must be a number")
            if not math.isfinite(float(value)):
                raise ValidationFailedError(f"metadata field '{name}' must be finite")
            normalized[name] = value
        elif field_type == "datetime":
            normalized[name] = _normalize_datetime(value, name)
        else:  # corrupted persisted registry: fail closed
            raise ValidationFailedError(f"metadata field '{name}' has an unsupported type")
    if _json_size(normalized) > MAX_USER_METADATA_BYTES:
        raise ValidationFailedError("metadata patch exceeds 16 KiB")
    return normalized, remove


class DocumentMetadataManager:
    def __init__(self, knowledge_service: Any) -> None:
        self._ks = knowledge_service
        pool = getattr(knowledge_service.db, "_pool", None)
        self._store = DocumentMetadataStore(pool)

    async def get_registry(
        self,
        user: UserContext,
        dataset_id: str,
    ) -> dict[str, Any]:
        await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        registry = await self._store.get_registry(dataset_id)
        if registry is None:
            raise ValidationFailedError("dataset not found")
        return registry

    async def update_registry(
        self,
        user: UserContext,
        dataset_id: str,
        *,
        expected_revision: int,
        fields: Iterable[Mapping[str, Any]],
    ) -> dict[str, Any]:
        await self._ks.require_dataset_access(user, dataset_id, required="owner")
        current = await self._store.get_registry(dataset_id)
        if current is None:
            raise ValidationFailedError("dataset not found")
        if current["revision"] != expected_revision:
            raise MetadataRegistryRevisionConflict("metadata schema changed; refresh and retry")
        normalized = validate_registry_fields(
            fields,
            previous_fields=current["fields"],
        )
        return await self._store.update_registry(
            dataset_id=dataset_id,
            expected_revision=expected_revision,
            fields=normalized,
        )

    async def patch_document(
        self,
        user: UserContext,
        dataset_id: str,
        document_id: str,
        *,
        metadata_patch: Mapping[str, Any],
        metadata_remove: Iterable[str],
        metadata_schema_revision: int,
        authorized_dataset: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]:
        if authorized_dataset is None:
            await self._ks.require_dataset_access(
                user,
                dataset_id,
                required="editor",
            )
        lease_factory = getattr(self._ks.db, "document_index_update_lease", None)
        if not callable(lease_factory):
            raise ValidationFailedError("document metadata editing requires PostgreSQL leases")

        async with (
            lease_factory(dataset_id, document_id) as connection,
            connection.transaction(),
        ):
            registry = await self._store.get_registry_locked(
                connection,
                dataset_id,
                for_update=False,
            )
            if registry is None:
                raise ValidationFailedError("dataset not found")
            if registry["revision"] != metadata_schema_revision:
                raise MetadataRegistryRevisionConflict("metadata schema changed; refresh and retry")
            normalized_patch, normalized_remove = normalize_metadata_patch(
                registry,
                metadata_patch,
                metadata_remove,
            )
            document = await self._ks.db.get_document(
                document_id,
                connection=connection,
            )
            if not document or str(document.get("dataset_id") or "") != dataset_id:
                raise LookupError("document not found")
            metadata = document.get("metadata")
            if metadata is None:
                metadata = {}
            if not isinstance(metadata, dict):
                raise ValidationFailedError("document metadata is malformed")
            # Start with every unknown/legacy user field. Only registered
            # keys explicitly present in patch/remove change. Source and
            # lifecycle fields are never copied from the request surface;
            # update_document_fields preserves their authoritative values.
            user_metadata = {
                key: value
                for key, value in metadata.items()
                if key not in SOURCE_OWNED_DOCUMENT_METADATA_KEYS
            }
            for name in normalized_remove:
                user_metadata.pop(name, None)
            user_metadata.update(normalized_patch)
            if _json_size(user_metadata) > MAX_USER_METADATA_BYTES:
                raise ValidationFailedError("document user metadata exceeds 16 KiB")
            await self._ks.db.update_document_fields(
                document_id,
                {"metadata": user_metadata},
                connection=connection,
            )
            updated = await self._ks.db.get_document(
                document_id,
                connection=connection,
            )
            if updated is None:
                raise RuntimeError("updated document metadata is not readable")
            return updated

    async def patch_documents(
        self,
        user: UserContext,
        dataset_id: str,
        document_ids: Iterable[str],
        *,
        metadata_patch: Mapping[str, Any],
        metadata_remove: Iterable[str],
        metadata_schema_revision: int,
    ) -> dict[str, Any]:
        dataset = await self._ks.require_dataset_access(
            user,
            dataset_id,
            required="editor",
        )
        normalized_ids = list(
            dict.fromkeys(str(document_id or "").strip() for document_id in document_ids)
        )
        normalized_ids = [document_id for document_id in normalized_ids if document_id]
        success_count = 0
        failed_ids: list[str] = []
        errors: dict[str, str] = {}
        for document_id in normalized_ids:
            try:
                await self.patch_document(
                    user,
                    dataset_id,
                    document_id,
                    metadata_patch=metadata_patch,
                    metadata_remove=metadata_remove,
                    metadata_schema_revision=metadata_schema_revision,
                    authorized_dataset=dataset,
                )
                success_count += 1
            except MetadataRegistryRevisionConflict:
                raise
            except LookupError:
                failed_ids.append(document_id)
                errors[document_id] = "not_found"
            except ValidationFailedError as exc:
                failed_ids.append(document_id)
                errors[document_id] = str(exc)[:500]
            except Exception:
                failed_ids.append(document_id)
                errors[document_id] = "update_failed"
        return {
            "success_count": success_count,
            "failed_count": len(failed_ids),
            "failed_ids": failed_ids,
            "errors": errors,
            "metadata_schema_revision": metadata_schema_revision,
        }
