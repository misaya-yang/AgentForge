"""Gateway-owned Agent data-governance cleanup.

Agent Studio deletion is control-plane work, not an Agent loop capability.  The
old implementation delegated the inventory and deletion to the removed Python
assistant service.  This module keeps the frozen-plan/receipt contract while
performing the scoped SQL and vector cleanup in the Gateway's data plane.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from collections.abc import Mapping
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote

import httpx
from ai_gateway_core.agents.deletion import (
    RUNTIME_CLEANUP_INVENTORY_SCHEMA,
    RUNTIME_CLEANUP_RECEIPT_SCHEMA,
    canonical_cleanup_digest,
    is_memory_source_handle,
    validate_runtime_cleanup_inventory,
    validate_runtime_cleanup_plan,
    validate_runtime_cleanup_receipt,
)

_MAX_PRINCIPALS = 10_000
_MAX_SOURCES = 20_000
_MAX_VECTORS = _MAX_SOURCES * 20
_SOURCE_TYPE_RE = re.compile(r"^[a-z][a-z0-9_]{0,127}$")


class AgentRuntimeCleanupClientError(RuntimeError):
    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


def _row_value(row: Any, key: str, default: Any = None) -> Any:
    if isinstance(row, Mapping):
        return row.get(key, default)
    try:
        return row[key]
    except (KeyError, IndexError, TypeError):
        return getattr(row, key, default)


def _utc(value: object) -> datetime | None:
    if isinstance(value, datetime):
        parsed = value
    else:
        raw = str(value or "").strip()
        if not raw:
            return None
        try:
            parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        except ValueError:
            return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _timestamp(value: object) -> str:
    parsed = _utc(value)
    return parsed.isoformat() if parsed else str(value or "")


def _scope_handle(tenant_id: str, principal_id: str, label: str) -> str:
    material = f"{tenant_id}\0{principal_id}\0{label}".encode()
    return f"memscope_{hashlib.sha256(material).hexdigest()[:32]}"


class _LegacyMemoryFiles:
    """Resolve only SQL-proven files below the isolated memory volume."""

    _SAFE = re.compile(r"[^a-zA-Z0-9_.-]+")

    def __init__(self) -> None:
        self.base = Path(os.getenv("AGENT_RUNTIME_MEMORY_DIR", "/app/agent-memory")).resolve()

    @classmethod
    def _legacy_component(cls, value: str) -> str:
        return cls._SAFE.sub("_", str(value or "").strip()).strip("._") or "unknown"

    @classmethod
    def _current_component(cls, value: str) -> str:
        raw = str(value or "").strip()
        cleaned = cls._SAFE.sub("_", raw).strip("._") or "unknown"
        return f"~{cleaned[:80]}-{hashlib.sha256(raw.encode()).hexdigest()}"

    def _roots(self, tenant_id: str, principal_id: str) -> tuple[Path, ...]:
        return (
            self.base / self._current_component(tenant_id) / self._current_component(principal_id),
            self.base / self._legacy_component(tenant_id) / self._legacy_component(principal_id),
        )

    def _candidate(self, tenant_id: str, principal_id: str, stored_path: str) -> Path | None:
        stored = Path(stored_path)
        roots = self._roots(tenant_id, principal_id)
        for root in roots:
            try:
                relative = stored.resolve().relative_to(root)
            except (OSError, ValueError):
                continue
            if relative.parts and all(part not in {"", ".", ".."} for part in relative.parts):
                return root / relative
        # The database may contain an absolute path from the old mount.  Accept
        # one, and only one, tenant/user suffix; never search arbitrary paths.
        tenant_pairs = {
            (self._legacy_component(tenant_id), self._legacy_component(principal_id)),
            (self._current_component(tenant_id), self._current_component(principal_id)),
        }
        parts = stored.parts
        matches = [
            i for i in range(max(0, len(parts) - 1))
            if tuple(parts[i : i + 2]) in tenant_pairs
        ]
        if len(matches) != 1 or matches[0] + 2 >= len(parts):
            return None
        tenant, principal = parts[matches[0] : matches[0] + 2]
        return self.base / tenant / principal / Path(*parts[matches[0] + 2 :])

    @staticmethod
    def _safe_file(path: Path, root: Path) -> bool:
        try:
            path.relative_to(root)
            root_stat = root.stat()
        except (OSError, ValueError):
            return False
        if root.is_symlink() or not root.is_dir():
            return False
        current = root
        for component in path.relative_to(root).parts[:-1]:
            current /= component
            try:
                if current.is_symlink() or not current.is_dir():
                    return False
            except OSError:
                return False
        try:
            if path.exists() and (path.is_symlink() or not path.is_file()):
                return False
            if path.is_symlink():
                return False
        except OSError:
            return False
        return root_stat.st_mode & 0o170000 == 0o040000

    @staticmethod
    def _generation(path: Path, relative: Path, legacy: bool) -> str:
        stat = path.stat()
        content = path.read_bytes()
        material = (
            stat.st_dev,
            stat.st_ino,
            stat.st_size,
            stat.st_mtime_ns,
            stat.st_ctime_ns,
        )
        generation = ":".join(str(item) for item in material) + f":{hashlib.sha256(content).hexdigest()}"
        handle_material = f"{'legacy/' if legacy else ''}{relative.as_posix()}\0{generation}".encode()
        return f"memsrc_{hashlib.sha256(handle_material).hexdigest()[:32]}"

    def resolve(
        self,
        tenant_id: str,
        principal_id: str,
        stored_path: str,
        expected_handle: str,
        *,
        allow_absent: bool,
    ) -> dict[str, Any] | None:
        candidate = self._candidate(tenant_id, principal_id, stored_path)
        if candidate is None:
            raise AgentRuntimeCleanupClientError("memory_source_file_scope_unproven")
        root = next((root for root in self._roots(tenant_id, principal_id) if candidate == root or root in candidate.parents), None)
        if root is None or not self._safe_file(candidate, root):
            raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
        # A missing path is an idempotently absent file only when the SQL row is
        # already a deletion tombstone; active rows must not hide lost data.
        if not candidate.exists():
            markers = [
                candidate.parent / f".{candidate.name}.{expected_handle}.deleting",
                candidate.parent / f".{candidate.name}.{expected_handle}.finalizing",
            ]
            if any(marker.is_symlink() for marker in markers):
                raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
            present_markers = [
                marker
                for marker in markers
                if marker.exists() and not marker.is_symlink() and marker.is_file()
            ]
            if len(present_markers) > 1:
                raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
            if present_markers:
                marker = present_markers[0]
                if not self._safe_file(marker, root):
                    raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
                actual = self._generation(
                    marker,
                    candidate.relative_to(root),
                    root == self._roots(tenant_id, principal_id)[1],
                )
                if actual != expected_handle:
                    raise AgentRuntimeCleanupClientError("memory_source_file_changed_since_prepare")
                return {"path": marker, "generation": actual, "absent": False}
            if not allow_absent:
                raise AgentRuntimeCleanupClientError("memory_source_file_missing")
            return {"path": candidate, "generation": None, "absent": True}
        if any(
            marker.exists() and not marker.is_symlink()
            for marker in (
                candidate.parent / f".{candidate.name}.{expected_handle}.deleting",
                candidate.parent / f".{candidate.name}.{expected_handle}.finalizing",
            )
        ):
            raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
        if candidate.suffix.lower() != ".md" or not candidate.is_file():
            raise AgentRuntimeCleanupClientError("memory_source_file_invalid")
        relative = candidate.relative_to(root)
        actual = self._generation(candidate, relative, root == self._roots(tenant_id, principal_id)[1])
        if actual != expected_handle:
            raise AgentRuntimeCleanupClientError("memory_source_file_changed_since_prepare")
        return {"path": candidate, "generation": actual, "absent": False}

    def delete_and_verify(self, item: Mapping[str, Any]) -> bool:
        path = Path(str(item["path"]))
        if not item.get("absent"):
            root = self.base
            if not self._safe_file(path, root):
                raise AgentRuntimeCleanupClientError("memory_source_file_unsafe")
            try:
                path.unlink()
            except FileNotFoundError:
                pass
            except OSError as exc:
                raise AgentRuntimeCleanupClientError("memory_source_file_delete_failed") from exc
        return not path.exists() and not path.is_symlink()


class _VectorCleanup:
    """Small, bounded Qdrant surface used only for frozen memory points."""

    def __init__(self) -> None:
        self.base_url = (
            os.getenv("GATEWAY_KNOWLEDGE__QDRANT__URL", "")
            or os.getenv("KNOWLEDGE_QDRANT__URL", "")
        ).strip().rstrip("/")
        self.api_key = (
            os.getenv("GATEWAY_KNOWLEDGE__QDRANT__API_KEY", "")
            or os.getenv("KNOWLEDGE_QDRANT__API_KEY", "")
        ).strip() or None

    def _headers(self) -> dict[str, str]:
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["api-key"] = self.api_key
        return headers

    async def delete_and_verify(
        self,
        *,
        collection: str,
        point_ids: list[str],
        tenant_id: str,
        principal_id: str,
    ) -> bool:
        if not point_ids:
            return True
        if not self.base_url:
            raise AgentRuntimeCleanupClientError("memory_vector_cleanup_unavailable")
        timeout = httpx.Timeout(connect=3.0, read=15.0, write=10.0, pool=3.0)
        path = f"/collections/{quote(collection, safe='')}/points/delete"
        filter_payload = {
            "filter": {
                "must": [
                    {"key": "tenant_id", "match": {"value": tenant_id}},
                    {"key": "user_id", "match": {"value": principal_id}},
                    {"has_id": point_ids},
                ]
            }
        }
        try:
            async with httpx.AsyncClient(base_url=self.base_url, timeout=timeout) as client:
                response = await client.post(
                    path,
                    params={"wait": "true"},
                    headers=self._headers(),
                    json=filter_payload,
                )
                if response.status_code not in {200, 404}:
                    response.raise_for_status()
                if response.status_code == 404:
                    return True
                readback = await client.post(
                    f"/collections/{quote(collection, safe='')}/points",
                    headers=self._headers(),
                    json={"ids": point_ids, "with_payload": False, "with_vector": False},
                )
                if readback.status_code == 404:
                    return True
                readback.raise_for_status()
                body = readback.json()
        except AgentRuntimeCleanupClientError:
            raise
        except (httpx.HTTPError, ValueError, TypeError) as exc:
            raise AgentRuntimeCleanupClientError("memory_vector_delete_failed") from exc
        result = body.get("result") if isinstance(body, Mapping) else None
        if isinstance(result, Mapping):
            result = result.get("points")
        return not any(isinstance(item, Mapping) for item in (result or []))


class _GatewayRuntimeMemoryCleanup:
    """Authoritative SQL/vector cleanup, with no filesystem or loop fallback."""

    def __init__(self, database: Any) -> None:
        self.database = database
        self.vectors = _VectorCleanup()
        self.files = _LegacyMemoryFiles()

    async def _source_rows(self, tenant_id: str, principals: list[str]) -> list[Any]:
        try:
            return await self.database.fetch(
                """
                SELECT source_id, tenant_id, user_id, source_path, source_type,
                       content_hash, metadata, created_at, updated_at
                FROM assistant_memory_sources
                WHERE tenant_id = $1 AND user_id = ANY($2::varchar[])
                  AND LOWER(COALESCE(metadata->>'deletion_completed', 'false')) <> 'true'
                ORDER BY user_id, source_id
                """,
                tenant_id,
                principals,
            )
        except Exception as exc:
            raise AgentRuntimeCleanupClientError("memory_cleanup_manifest_unavailable") from exc

    async def _chunk_rows(self, tenant_id: str, source_ids: list[str]) -> list[Any]:
        if not source_ids:
            return []
        try:
            return await self.database.fetch(
                """
                SELECT source_id, vector_id
                FROM assistant_memory_chunks
                WHERE tenant_id = $1 AND source_id = ANY($2::uuid[])
                  AND vector_id IS NOT NULL
                ORDER BY source_id, vector_id
                """,
                tenant_id,
                source_ids,
            )
        except Exception as exc:
            raise AgentRuntimeCleanupClientError("memory_cleanup_manifest_unavailable") from exc

    @staticmethod
    def _source_handle(metadata: object) -> str:
        if not isinstance(metadata, Mapping):
            raise AgentRuntimeCleanupClientError("memory_cleanup_manifest_invalid")
        handle = str(metadata.get("source_handle") or metadata.get("deletion_source_handle") or "")
        if not is_memory_source_handle(handle):
            raise AgentRuntimeCleanupClientError("memory_cleanup_source_handle_unavailable")
        return handle

    async def _manifests(
        self,
        *,
        tenant_id: str,
        principals: list[str],
        cutoff: datetime | None,
    ) -> dict[str, list[dict[str, Any]]]:
        rows = await self._source_rows(tenant_id, principals)
        source_ids = [str(_row_value(row, "source_id") or "") for row in rows]
        chunk_rows = await self._chunk_rows(tenant_id, source_ids)
        vectors_by_source: dict[str, list[str]] = {}
        for row in chunk_rows:
            source_id = str(_row_value(row, "source_id") or "")
            vector_id = str(_row_value(row, "vector_id") or "")
            if vector_id:
                vectors_by_source.setdefault(source_id, []).append(vector_id)

        manifests: dict[str, list[dict[str, Any]]] = {principal: [] for principal in principals}
        for row in rows:
            principal = str(_row_value(row, "user_id") or "")
            if principal not in manifests:
                continue
            updated_at = _utc(_row_value(row, "updated_at"))
            if cutoff is not None and (updated_at is None or updated_at > cutoff):
                continue
            source_id = str(_row_value(row, "source_id") or "")
            label = str(_row_value(row, "source_path") or "").replace("\\", "/").rstrip("/").rsplit("/", 1)[-1]
            source_type = str(_row_value(row, "source_type") or "unknown")
            if not label or label in {".", ".."} or "/" in label or "\\" in label or not _SOURCE_TYPE_RE.fullmatch(source_type):
                raise AgentRuntimeCleanupClientError("memory_cleanup_manifest_invalid")
            metadata = _row_value(row, "metadata", {})
            if isinstance(metadata, str):
                try:
                    metadata = json.loads(metadata)
                except (TypeError, ValueError) as exc:
                    raise AgentRuntimeCleanupClientError("memory_cleanup_manifest_invalid") from exc
            handle = self._source_handle(metadata)
            file_state = self.files.resolve(
                tenant_id,
                principal,
                str(_row_value(row, "source_path") or ""),
                handle,
                allow_absent=(
                    str(metadata.get("deletion_pending") or "").lower() == "true"
                    or str(metadata.get("deletion_completed") or "").lower() == "true"
                ),
            )
            version = canonical_cleanup_digest(
                {
                    "source_handle": handle,
                    "source_type": source_type,
                    "content_hash": str(_row_value(row, "content_hash") or ""),
                    "updated_at": _timestamp(_row_value(row, "updated_at")),
                    "file_generation": file_state["generation"],
                }
            )
            collections = metadata.get("vector_collections", []) if isinstance(metadata, Mapping) else []
            if not isinstance(collections, list):
                raise AgentRuntimeCleanupClientError("memory_vector_collection_inventory_invalid")
            point_ids = sorted(set(vectors_by_source.get(source_id, [])))
            if point_ids and not collections:
                raise AgentRuntimeCleanupClientError("memory_vector_collection_inventory_invalid")
            vector_sets: list[dict[str, Any]] = []
            for collection in sorted({str(item) for item in collections if str(item).strip()}):
                points = [
                    {
                        "point_id": point_id,
                        "version_digest": canonical_cleanup_digest(
                            {"point_id": point_id, "source_version": version}
                        ),
                    }
                    for point_id in point_ids
                ]
                vector_sets.append(
                    {
                        "collection_kind": "current",
                        "collection_handle": "memvec_" + hashlib.sha256(collection.encode()).hexdigest()[:32],
                        "point_count": len(points),
                        "points": points,
                        "_collection": collection,
                    }
                )
            manifests[principal].append(
                {
                    "logical_source_scope_handle": _scope_handle(tenant_id, principal, label),
                    "source_handle": handle,
                    "version_digest": version,
                    "source_type": source_type,
                    "_source_id": source_id,
                    "_file": file_state,
                    "_vector_sets": vector_sets,
                }
            )
        return manifests

    async def inspect(self, plan_value: object) -> dict[str, Any]:
        plan = validate_runtime_cleanup_plan(plan_value)
        if len(plan["principal_handles"]) > _MAX_PRINCIPALS:
            raise AgentRuntimeCleanupClientError("memory_cleanup_scope_too_large")
        cutoff = _utc(plan["cutoff_at"])
        if cutoff is None:
            raise AgentRuntimeCleanupClientError("memory_cleanup_cutoff_invalid")
        manifests = await self._manifests(
            tenant_id=plan["tenant_id"],
            principals=list(plan["principal_handles"]),
            cutoff=cutoff,
        )
        public_principals: list[dict[str, Any]] = []
        total_sources = total_vectors = 0
        for principal in plan["principal_handles"]:
            source_items = manifests[principal]
            public_sources = [
                {key: item[key] for key in ("logical_source_scope_handle", "source_handle", "version_digest", "source_type")}
                for item in source_items
            ]
            public_sources.sort(key=lambda item: item["logical_source_scope_handle"])
            vectors_by_handle: dict[str, dict[str, Any]] = {}
            for item in source_items:
                for vector in item["_vector_sets"]:
                    handle = str(vector["collection_handle"])
                    target = vectors_by_handle.setdefault(
                        handle,
                        {
                            "collection_kind": vector["collection_kind"],
                            "collection_handle": handle,
                            "point_count": 0,
                            "points": [],
                        },
                    )
                    known_points = {str(point["point_id"]) for point in target["points"]}
                    for point in vector["points"]:
                        if str(point["point_id"]) not in known_points:
                            target["points"].append(point)
                            known_points.add(str(point["point_id"]))
            public_vectors = sorted(vectors_by_handle.values(), key=lambda item: item["collection_handle"])
            for vector in public_vectors:
                vector["points"].sort(key=lambda item: item["point_id"])
                vector["point_count"] = len(vector["points"])
            vector_count = sum(int(item["point_count"]) for item in public_vectors)
            total_sources += len(public_sources)
            total_vectors += vector_count
            if total_sources > _MAX_SOURCES or total_vectors > _MAX_VECTORS:
                raise AgentRuntimeCleanupClientError("memory_cleanup_scope_too_large")
            public_principals.append(
                {
                    "principal_id": principal,
                    "source_count": len(public_sources),
                    "sources": public_sources,
                    "vector_count": vector_count,
                    "vector_sets": public_vectors,
                }
            )
        inventory: dict[str, Any] = {
            "schema_version": RUNTIME_CLEANUP_INVENTORY_SCHEMA,
            "deletion_id": plan["deletion_id"],
            "tenant_id": plan["tenant_id"],
            "agent_id": plan["agent_id"],
            "plan_digest": plan["plan_digest"],
            "cutoff_at": plan["cutoff_at"],
            "principal_count": len(public_principals),
            "source_count": total_sources,
            "vector_count": total_vectors,
            "principals": public_principals,
        }
        inventory["inventory_digest"] = canonical_cleanup_digest(inventory)
        return inventory

    async def execute(self, *, plan_value: object, inventory_value: object) -> dict[str, Any]:
        plan = validate_runtime_cleanup_plan(plan_value)
        inventory = validate_runtime_cleanup_inventory(inventory_value, plan=plan)
        current_by_principal = await self._manifests(
            tenant_id=plan["tenant_id"],
            principals=list(plan["principal_handles"]),
            cutoff=None,
        )
        receipts: list[dict[str, Any]] = []
        for frozen_principal in inventory["principals"]:
            principal_id = str(frozen_principal["principal_id"])
            frozen_sources = {str(item["source_handle"]): item for item in frozen_principal["sources"]}
            current_sources = {str(item["source_handle"]): item for item in current_by_principal[principal_id]}
            source_count = int(frozen_principal["source_count"])
            vector_count = int(frozen_principal["vector_count"])
            deleted_sources = absent_sources = deleted_vectors = absent_vectors = 0
            errors: list[str] = []
            frozen_vector_sets = {
                str(item["collection_handle"]): item
                for item in frozen_principal["vector_sets"]
            }
            current_vector_sets: dict[str, dict[str, Any]] = {}
            for source in current_sources.values():
                for item in source["_vector_sets"]:
                    handle = str(item["collection_handle"])
                    target = current_vector_sets.setdefault(
                        handle,
                        {
                            "_collection": item["_collection"],
                            "points": [],
                        },
                    )
                    known_points = {str(point["point_id"]) for point in target["points"]}
                    for point in item["points"]:
                        if str(point["point_id"]) not in known_points:
                            target["points"].append(point)
                            known_points.add(str(point["point_id"]))
            vector_failed = False
            for collection_handle, vector_set in frozen_vector_sets.items():
                current_set = current_vector_sets.get(collection_handle)
                if current_set is None:
                    absent_vectors += int(vector_set["point_count"])
                    continue
                current_ids = {
                    str(point["point_id"]): point for point in current_set["points"]
                }
                point_ids: list[str] = []
                for point in vector_set["points"]:
                    current_point = current_ids.get(str(point["point_id"]))
                    if current_point is None:
                        absent_vectors += 1
                    elif current_point["version_digest"] != point["version_digest"]:
                        errors.append("memory_vector_changed_since_prepare")
                        vector_failed = True
                    else:
                        point_ids.append(str(point["point_id"]))
                if point_ids and not vector_failed:
                    try:
                        verified = await self.vectors.delete_and_verify(
                            collection=str(current_set["_collection"]),
                            point_ids=point_ids,
                            tenant_id=plan["tenant_id"],
                            principal_id=principal_id,
                        )
                    except AgentRuntimeCleanupClientError as exc:
                        errors.append(exc.code)
                        vector_failed = True
                    else:
                        if verified:
                            deleted_vectors += len(point_ids)
                        else:
                            errors.append("memory_vector_delete_readback_failed")
                            vector_failed = True
            for handle, frozen in frozen_sources.items():
                current = current_sources.get(handle)
                if current is None:
                    absent_sources += 1
                    continue
                if current["version_digest"] != frozen["version_digest"]:
                    errors.append("memory_source_changed_since_prepare")
                    continue
                if vector_failed:
                    continue
                try:
                    if not self.files.delete_and_verify(current["_file"]):
                        errors.append("memory_source_file_delete_readback_failed")
                        continue
                except AgentRuntimeCleanupClientError as exc:
                    errors.append(exc.code)
                    continue
                try:
                    await self.database.execute(
                        """
                        DELETE FROM assistant_memory_sources
                        WHERE tenant_id = $1 AND user_id = $2 AND source_id = $3::uuid
                          AND (metadata->>'source_handle' = $4 OR metadata->>'deletion_source_handle' = $4)
                        """,
                        plan["tenant_id"],
                        principal_id,
                        str(current["_source_id"]),
                        handle,
                    )
                    remaining = await self.database.fetch(
                        """
                        SELECT source_id FROM assistant_memory_sources
                        WHERE tenant_id = $1 AND user_id = $2 AND source_id = $3::uuid
                        """,
                        plan["tenant_id"],
                        principal_id,
                        str(current["_source_id"]),
                    )
                except Exception:
                    errors.append("memory_source_delete_failed")
                    continue
                if remaining:
                    errors.append("memory_source_delete_readback_failed")
                else:
                    deleted_sources += 1
            errors = list(dict.fromkeys(errors))
            completed = (
                not errors
                and deleted_sources + absent_sources == source_count
                and deleted_vectors + absent_vectors == vector_count
            )
            receipts.append(
                {
                    "principal_id": principal_id,
                    "status": "completed" if completed else "partial",
                    "completed": completed,
                    "retryable": not completed,
                    "source_count": source_count,
                    "deleted_source_count": deleted_sources,
                    "vector_count": vector_count,
                    "deleted_vector_count": deleted_vectors,
                    "idempotent_absent_count": absent_sources,
                    "idempotent_absent_vector_count": absent_vectors,
                    "errors": errors or ([] if completed else ["memory_cleanup_incomplete"]),
                }
            )
        completed = all(item["completed"] for item in receipts)
        receipt: dict[str, Any] = {
            "schema_version": RUNTIME_CLEANUP_RECEIPT_SCHEMA,
            "deletion_id": plan["deletion_id"],
            "tenant_id": plan["tenant_id"],
            "agent_id": plan["agent_id"],
            "plan_digest": plan["plan_digest"],
            "inventory_digest": inventory["inventory_digest"],
            "status": "completed" if completed else "partial",
            "completed": completed,
            "retryable": not completed,
            "principals": receipts,
            "errors": sorted({error for item in receipts for error in item["errors"]}),
        }
        receipt["receipt_digest"] = canonical_cleanup_digest(receipt)
        return receipt


class AgentRuntimeCleanupClient:
    """Compatibility-named facade for the Gateway-owned cleanup authority."""

    def __init__(self, database: Any | None = None) -> None:
        self._service = _GatewayRuntimeMemoryCleanup(database) if database is not None else None

    async def inspect(self, plan_value: object) -> dict[str, Any]:
        if self._service is None:
            raise AgentRuntimeCleanupClientError("AGENT_RUNTIME_CLEANUP_STORAGE_UNAVAILABLE")
        try:
            value = await self._service.inspect(plan_value)
            plan = validate_runtime_cleanup_plan(plan_value)
            return validate_runtime_cleanup_inventory(value, plan=plan)
        except AgentRuntimeCleanupClientError:
            raise
        except (TypeError, ValueError) as exc:
            raise AgentRuntimeCleanupClientError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID") from exc

    async def execute(self, *, plan_value: object, inventory_value: object) -> dict[str, Any]:
        if self._service is None:
            raise AgentRuntimeCleanupClientError("AGENT_RUNTIME_CLEANUP_STORAGE_UNAVAILABLE")
        try:
            value = await self._service.execute(plan_value=plan_value, inventory_value=inventory_value)
            plan = validate_runtime_cleanup_plan(plan_value)
            inventory = validate_runtime_cleanup_inventory(inventory_value, plan=plan)
            return validate_runtime_cleanup_receipt(value, plan=plan, inventory=inventory)
        except AgentRuntimeCleanupClientError:
            raise
        except (TypeError, ValueError) as exc:
            raise AgentRuntimeCleanupClientError("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID") from exc


__all__ = ["AgentRuntimeCleanupClient", "AgentRuntimeCleanupClientError"]
