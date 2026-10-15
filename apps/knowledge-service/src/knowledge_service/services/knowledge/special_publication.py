"""Durable cross-collection publication for hierarchy and scanned PDF plans."""

from __future__ import annotations

import hashlib
import json
import re
import uuid
from typing import Any

from ...persistence.database import dataset_ingestion_identity
from .lexical_config import LexicalConfig

SOURCE_MANIFEST_KEY = "_special_source_manifest"


class SpecialSourceUnverifiableError(RuntimeError):
    """An old serving image cannot be pinned to immutable bytes (HTTP 409)."""

    status_code = 409


def _sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, str):
        value = json.loads(value)
    return value if isinstance(value, dict) else {}


class SpecialPublicationCoordinator:
    """Publish an immutable candidate while the dataset revision is negative."""

    def __init__(self, service: Any) -> None:
        self.service = service
        self.db = service.db
        self.vector_store = service.vector_store

    async def _active_context(
        self, dataset_id: str, collection: str,
    ) -> dict[str, Any] | None:
        lifecycle = getattr(self.service, "bm25_v2_lifecycle_service", None)
        if lifecycle is not None:
            return await lifecycle.active_publication_context(dataset_id)
        get_profile = getattr(self.vector_store, "get_live_lexical_profile", None)
        if callable(get_profile) and collection:
            profile, _receipt = await get_profile(collection)
            if profile is not None and profile.reads_bm25_v2:
                raise RuntimeError("active BM25 v2 publication requires lifecycle authority")
        return None

    @staticmethod
    def _point_plan(plan: Any) -> dict[str, list[str]]:
        result: dict[str, list[str]] = {}
        for collection, points in plan.points_by_collection.items():
            name = str(collection or "").strip()
            ids = [str(point.id) for point in points]
            if not name or len(ids) != len(set(ids)) or not all(ids):
                raise ValueError("special candidate point identities are invalid")
            result[name] = sorted(ids)
        if not result or not any(result.values()):
            raise ValueError("special candidate has no points")
        if len({point_id for ids in result.values() for point_id in ids}) != sum(
            len(ids) for ids in result.values()
        ):
            raise ValueError("special candidate point ids overlap collections")
        return result

    @staticmethod
    def _object_plan(plan: Any) -> dict[str, str]:
        objects: dict[str, str] = {}
        for receipt in plan.object_manifest:
            key = str(receipt.get("storage_key") or "").strip()
            digest = str(receipt.get("sha256") or "").strip().lower()
            if not key or not re.fullmatch(r"[0-9a-f]{64}", digest) or key in objects:
                raise ValueError("special candidate object receipt is invalid")
            objects[key] = digest
        return dict(sorted(objects.items()))

    @staticmethod
    def _plan_hash(
        plan: Any, objects: dict[str, str], source_manifest: dict[str, Any],
    ) -> str:
        # URLs can expire between retries; the storage key and byte hash are
        # immutable. Include vectors and row contents so a replay cannot reuse
        # deterministic IDs for changed embeddings or text.
        rows = [
            {key: value for key, value in row.items() if key != "image_url"}
            for row in plan.segment_rows
        ]
        points = {
            name: [point.model_dump(mode="json", exclude_none=True) for point in values]
            for name, values in sorted(plan.points_by_collection.items())
        }
        canonical = json.dumps(
            {"source_hash": plan.source_hash, "content": plan.content,
             "points": points, "rows": rows, "objects": objects,
             "source_manifest": source_manifest,
             "summary": getattr(plan, "summary_row", None)},
            sort_keys=True, separators=(",", ":"), ensure_ascii=False, default=str,
        )
        return _sha256_text(canonical)

    @staticmethod
    def _source_kind(plan: Any, objects: dict[str, str]) -> str:
        inferred = (
            "vision" if getattr(plan, "total_pages", None) is not None or objects
            else "hierarchy"
        )
        declared = str(getattr(plan, "source_kind", "") or "")
        if declared == "text":
            if inferred != "hierarchy":
                raise SpecialSourceUnverifiableError("text candidate has image receipts")
            if plan.source_hash != _sha256_text(plan.content):
                raise SpecialSourceUnverifiableError("text candidate source hash differs")
            return "text"
        if declared and declared != inferred:
            raise SpecialSourceUnverifiableError("special candidate source kind differs")
        return inferred

    @staticmethod
    def _vision_page_texts(plan: Any, objects: dict[str, str]) -> list[dict[str, Any]]:
        extracted = getattr(plan, "extracted_texts", None)
        if not isinstance(extracted, dict) or not objects:
            raise SpecialSourceUnverifiableError("vision page text receipt is unavailable")
        pages: list[dict[str, Any]] = []
        seen: set[int] = set()
        generation_hex = uuid.UUID(str(plan.generation_id)).hex
        for receipt in plan.object_manifest:
            attachment = str(receipt.get("attachment_id") or "")
            match = re.fullmatch(r"page_([1-9][0-9]*)_g([0-9a-f]{32})", attachment)
            if not match or match.group(2) != generation_hex:
                raise SpecialSourceUnverifiableError("vision page generation identity differs")
            number = int(match.group(1))
            key = str(receipt.get("storage_key") or "")
            if number in seen or key not in objects:
                raise SpecialSourceUnverifiableError("vision page object receipt is incomplete")
            seen.add(number)
            page_text = extracted.get(number, "")
            if not isinstance(page_text, str):
                raise SpecialSourceUnverifiableError("vision page text is invalid")
            pages.append({
                "page_number": number, "text": page_text,
                "storage_key": key, "sha256": objects[key],
            })
        pages.sort(key=lambda page: page["page_number"])
        if len(pages) != int(getattr(plan, "total_pages", 0) or 0):
            raise SpecialSourceUnverifiableError("vision page receipt count differs")
        reconstructed = "\n\n".join(
            f"[Page {page['page_number']}]\n{page['text']}"
            for page in pages if page["text"]
        )
        if reconstructed != plan.content:
            raise SpecialSourceUnverifiableError("vision page texts differ from candidate content")
        return pages

    async def _old_source_manifest(
        self, current: dict[str, Any], document_id: str,
        *, tenant_id: str, index_config: dict[str, Any], connection: Any,
    ) -> dict[str, Any]:
        """Capture serving image bytes before replacing their segment rows."""

        metadata = _json_object(current["metadata"])
        previous = _json_object(metadata.get(SOURCE_MANIFEST_KEY))
        old_content = str(current["content"] or "")
        content_hash = _sha256_text(old_content)
        if previous.get("content_hash") not in {None, content_hash}:
            raise SpecialSourceUnverifiableError("old source content manifest differs")
        objects = _json_object(previous.get("objects"))
        for key, digest in objects.items():
            if not isinstance(key, str) or not re.fullmatch(r"[0-9a-f]{64}", str(digest)):
                raise SpecialSourceUnverifiableError("old image receipt is invalid")
        image_rows = await self.db.get_image_segments_by_document(
            document_id, connection=connection,
        )
        page_texts: list[dict[str, Any]] = []
        if image_rows:
            storage = getattr(self.service, "image_storage_service", None)
            generate_key = getattr(storage, "_generate_key", None)
            download = getattr(storage, "download_image", None)
            if not callable(generate_key) or not callable(download):
                raise SpecialSourceUnverifiableError("old image storage cannot be verified")
            for row in image_rows:
                attachment = str(row.get("image_attachment_id") or "").strip()
                filename = str(row.get("image_filename") or "").strip()
                if not attachment or not filename:
                    raise SpecialSourceUnverifiableError("old image object identity is missing")
                key = generate_key(tenant_id, document_id, attachment, filename)
                try:
                    content = await download(
                        tenant_id=tenant_id, document_id=document_id,
                        attachment_id=attachment, filename=filename,
                    )
                except Exception as exc:
                    raise SpecialSourceUnverifiableError(
                        "old image bytes cannot be read"
                    ) from exc
                actual_hash = hashlib.sha256(content).hexdigest()
                if key in objects and objects[key] != actual_hash:
                    raise SpecialSourceUnverifiableError("old image bytes changed")
                objects[key] = actual_hash
                row_metadata = _json_object(row.get("metadata"))
                attachment_page = re.fullmatch(r"page_([1-9][0-9]*)(?:_g[0-9a-f]{32})?", attachment)
                number = row_metadata.get("page_number")
                if type(number) is not int and attachment_page:
                    number = int(attachment_page.group(1))
                if type(number) is not int or number <= 0:
                    raise SpecialSourceUnverifiableError("old image page order is unavailable")
                marker = f"[Page {number}]"
                row_text = str(row.get("text") or "")
                if row_text == marker:
                    page_text = ""
                elif row_text.startswith(marker + "\n"):
                    page_text = row_text[len(marker) + 1:]
                else:
                    raise SpecialSourceUnverifiableError("old image page text is unavailable")
                page_texts.append({
                    "page_number": number, "text": page_text,
                    "storage_key": key, "sha256": actual_hash,
                })
            page_texts.sort(key=lambda page: page["page_number"])
            if len({page["page_number"] for page in page_texts}) != len(page_texts):
                raise SpecialSourceUnverifiableError("old image page order is ambiguous")
            reconstructed = "\n\n".join(
                f"[Page {page['page_number']}]\n{page['text']}"
                for page in page_texts if page["text"]
            )
            if reconstructed != old_content:
                raise SpecialSourceUnverifiableError("old image page texts differ from source")
        prior_kind = str(previous.get("source_kind") or "")
        if prior_kind and prior_kind not in {"hierarchy", "vision", "text"}:
            raise SpecialSourceUnverifiableError("old source kind is invalid")
        if image_rows and prior_kind not in {"", "vision"}:
            raise SpecialSourceUnverifiableError("old source kind and image rows differ")
        if prior_kind == "vision":
            prior_pages = previous.get("page_texts")
            expected_objects = {
                page["storage_key"]: page["sha256"] for page in page_texts
            }
            if (
                not isinstance(prior_pages, list)
                or prior_pages != page_texts
                or _json_object(previous.get("objects")) != expected_objects
            ):
                raise SpecialSourceUnverifiableError(
                    "old vision page receipts differ from serving image rows"
                )
        prior_index_config = previous.get("index_config", index_config)
        if not isinstance(prior_index_config, dict):
            raise SpecialSourceUnverifiableError("old source index config is invalid")
        if prior_kind == "text" and (
            previous.get("source_hash", content_hash) != content_hash
            or objects or previous.get("page_texts") not in (None, [])
        ):
            raise SpecialSourceUnverifiableError("old text source receipt differs")
        return {
            "generation_id": str(previous.get("generation_id") or ""),
            "source_hash": str(previous.get("source_hash") or content_hash),
            "content_hash": content_hash,
            "source_kind": prior_kind or ("vision" if image_rows else "text"),
            "original_source_key": str(previous.get("original_source_key") or metadata.get("original_file_key") or ""),
            "index_config": prior_index_config,
            "page_texts": page_texts,
            "objects": dict(sorted(objects.items())),
        }

    @staticmethod
    async def _set_version_manifest(
        connection: Any, document_id: str, version_number: int,
        source_manifest: dict[str, Any],
    ) -> None:
        row = await connection.fetchrow(
            """UPDATE document_versions
               SET metadata = COALESCE(metadata, '{}'::jsonb)
                   || jsonb_build_object($3::text, $4::jsonb)
               WHERE document_id = $1 AND version_number = $2
               RETURNING version_id""",
            document_id, version_number, SOURCE_MANIFEST_KEY,
            json.dumps(source_manifest, sort_keys=True),
        )
        if row is None:
            raise RuntimeError("special source version manifest could not be saved")

    async def _delete_points(
        self, collections: dict[str, dict[str, list[str]]], key: str,
        *, tenant_id: str, dataset_id: str, document_id: str,
    ) -> None:
        delete = getattr(self.vector_store, "delete_document_points_by_ids", None)
        if not callable(delete):
            raise RuntimeError("document-scoped point cleanup is unavailable")
        for collection, entries in sorted(collections.items()):
            ids = entries.get(key) or []
            if ids:
                await delete(
                    collection, ids, tenant_id=tenant_id, dataset_id=dataset_id,
                    document_id=document_id,
                    lifecycle_lease_held=True,
                )

    async def _delete_objects(
        self, keys: list[str], *, tenant_id: str, document_id: str, generation_id: str,
    ) -> None:
        if not keys:
            return
        storage = getattr(self.service, "image_storage_service", None)
        delete = getattr(storage, "delete_image", None)
        exists = getattr(storage, "image_exists", None)
        generate_key = getattr(storage, "_generate_key", None)
        if not callable(delete) or not callable(exists) or not callable(generate_key):
            raise RuntimeError("special candidate object cleanup is unavailable")
        generation_hex = uuid.UUID(generation_id).hex
        for key in keys:
            match = re.search(r"(?:^|/)(page_([1-9][0-9]*)_g([0-9a-f]{32}))_(page_([1-9][0-9]*)\.png)$", key)
            if not match or match.group(3) != generation_hex or match.group(2) != match.group(5):
                raise RuntimeError("special candidate object key is outside its generation")
            attachment_id, filename = match.group(1), match.group(4)
            if generate_key(tenant_id, document_id, attachment_id, filename) != key:
                raise RuntimeError("special candidate object key has another owner")
            identity = {
                "tenant_id": tenant_id, "document_id": document_id,
                "attachment_id": attachment_id, "filename": filename,
            }
            if not await exists(**identity):
                continue  # Preparing may have stopped before this page was uploaded.
            deleted = await delete(**identity)
            if deleted is not True or await exists(**identity):
                raise RuntimeError("special candidate object cleanup could not be verified")

    async def _finish_fence(
        self, dataset_id: str, revision: int, connection: Any,
        active_context: dict[str, Any] | None,
    ) -> None:
        lifecycle = getattr(self.service, "bm25_v2_lifecycle_service", None)
        certification = None
        if active_context is not None:
            if lifecycle is None:
                raise RuntimeError("active BM25 v2 publication authority is unavailable")
            certification = await lifecycle.recertify_active_publication(
                active_context, publication_revision=revision, connection=connection,
            )
        final_revision = await self.db.finish_index_publication(
            dataset_id, connection=connection,
        )
        if certification is not None:
            if final_revision != int(certification["target_revision"]):
                raise RuntimeError("active BM25 v2 publication revision disagrees")
            await lifecycle.settle_active_publication(
                active_context, certification, connection=connection,
            )

    async def _close_committed(
        self, manifest: dict[str, Any], revision: int, connection: Any,
        active_context: dict[str, Any] | None, *, tenant_id: str,
    ) -> None:
        await self._delete_points(
            manifest["collections"], "old_point_ids",
            tenant_id=tenant_id, dataset_id=manifest["dataset_id"],
            document_id=manifest["document_id"],
        )
        async with connection.transaction():
            result = await self.db.reconcile_special_publication_execution(
                manifest["execution_id"], manifest["document_id"],
                manifest["dataset_id"], manifest["generation_id"],
                connection=connection,
            )
            if result not in {"reconciled", "already_terminal"}:
                raise RuntimeError("committed special publication could not reconcile")
            await self._finish_fence(
                manifest["dataset_id"], revision, connection, active_context,
            )

    async def _abort_uncommitted(
        self, manifest: dict[str, Any], revision: int, connection: Any,
        active_context: dict[str, Any] | None, *, tenant_id: str,
    ) -> None:
        phase = str(manifest["phase"])
        if phase in {"prepared", "points_written"}:
            await self._delete_points(
                manifest["collections"], "candidate_point_ids",
                tenant_id=tenant_id, dataset_id=manifest["dataset_id"],
                document_id=manifest["document_id"],
            )
        await self._delete_objects(
            manifest.get("planned_object_keys") or [],
            tenant_id=tenant_id, document_id=manifest["document_id"],
            generation_id=manifest["generation_id"],
        )
        async with connection.transaction():
            await self.db.update_document_status(
                manifest["document_id"], status="error",
                error="special publication aborted after cleanup", connection=connection,
            )
            if phase != "aborted":
                updated = await self.db.advance_special_publication_manifest(
                    manifest["execution_id"], manifest["document_id"],
                    manifest["dataset_id"], manifest["generation_id"],
                    source_hash=manifest["source_hash"],
                    plan_hash=manifest.get("plan_hash"),
                    expected_phase=phase, next_phase="aborted", connection=connection,
                )
                manifest = {**manifest, **updated}
            result = await self.db.reconcile_special_publication_execution(
                manifest["execution_id"], manifest["document_id"],
                manifest["dataset_id"], manifest["generation_id"],
                connection=connection,
            )
            if result not in {"reconciled", "already_terminal"}:
                raise RuntimeError("aborted special publication could not reconcile")
            await self._finish_fence(
                manifest["dataset_id"], revision, connection, active_context,
            )

    async def _resume_preparing(
        self, manifest: dict[str, Any], revision: int, connection: Any,
        active_context: dict[str, Any] | None, *, tenant_id: str,
    ) -> None:
        """Retire only unpublished objects, then requeue the same execution."""

        if manifest.get("phase") != "preparing":
            raise RuntimeError("only an unplanned preparation may reuse its execution")
        await self._delete_objects(
            manifest.get("planned_object_keys") or [],
            tenant_id=tenant_id, document_id=manifest["document_id"],
            generation_id=manifest["generation_id"],
        )
        async with connection.transaction():
            await self.db.resume_special_preparing_execution(
                manifest["execution_id"], manifest["document_id"],
                manifest["dataset_id"], manifest["generation_id"],
                manifest["source_hash"], connection=connection,
            )
            await self._finish_fence(
                manifest["dataset_id"], revision, connection, active_context,
            )

    async def publish(
        self, plan: Any, dataset: dict[str, Any], execution_id: str,
        expected_content: str, *, metadata_patch: dict[str, Any] | None = None,
        document_shared_lease_held: bool = False,
    ) -> list[str]:
        """Publish a prepared plan; leave a negative revision if cleanup fails."""

        dataset_id = str(dataset.get("dataset_id") or "")
        tenant_id = str(dataset.get("tenant_id") or "")
        document_id = str(plan.document_id)
        generation_id = str(plan.generation_id)
        if not dataset_id or not tenant_id or dataset_id != plan.dataset_id:
            raise ValueError("special candidate dataset owner differs")
        point_ids = self._point_plan(plan)
        objects = self._object_plan(plan)
        recorded = await self.db.get_special_publication_manifest(execution_id)
        if (
            recorded is None or recorded.get("phase") != "preparing"
            or recorded.get("document_id") != document_id
            or recorded.get("dataset_id") != dataset_id
            or recorded.get("generation_id") != generation_id
            or recorded.get("source_hash") != plan.source_hash
            or sorted(recorded.get("planned_object_keys") or []) != sorted(objects)
        ):
            raise RuntimeError("special candidate has no matching preparing manifest")
        base = str(dataset.get("collection_name") or "")
        await self._active_context(dataset_id, base)
        identity = dataset_ingestion_identity(dataset)
        async with self.db.dataset_index_publication_lease(
            dataset_id, expected_ingestion_identity=identity,
            special_execution_id=execution_id,
            document_shared_lease_held=document_shared_lease_held,
        ) as publication:
            if publication.recovered:
                raise RuntimeError("negative special publication requires recovery")
            connection = publication.connection
            active_context = await self._active_context(dataset_id, base)
            committed = False
            manifest = recorded
            try:
                # Exact old IDs are frozen under the lease before any candidate
                # point upsert. Never reuse or overwrite one of them.
                old_ids = await self.vector_store.document_point_ids_by_collection(
                    tenant_id=tenant_id, dataset_id=dataset_id, document_id=document_id,
                )
                if any(set(ids) & set(old_ids.get(name, [])) for name, ids in point_ids.items()):
                    raise RuntimeError("special candidate would overwrite an old point")
                collections = {
                    name: {"candidate_point_ids": point_ids.get(name, []),
                           "old_point_ids": old_ids.get(name, [])}
                    for name in sorted(set(point_ids) | set(old_ids))
                }
                current = await connection.fetchrow(
                    "SELECT title, current_version, metadata, content FROM documents "
                    "WHERE document_id = $1 AND dataset_id = $2 FOR UPDATE",
                    document_id, dataset_id,
                )
                if current is None or str(current["content"] or "") != expected_content:
                    raise RuntimeError("special candidate source changed")
                metadata = _json_object(current["metadata"])
                restore = metadata.get("_document_pending_restore_version")
                content_hash = _sha256_text(plan.content)
                raw_index_config = dataset.get("index_config") or {}
                if not isinstance(raw_index_config, dict):
                    raise SpecialSourceUnverifiableError("special source index config is invalid")
                pinned_index_config = json.loads(json.dumps(
                    raw_index_config, sort_keys=True, ensure_ascii=False,
                ))
                source_kind = self._source_kind(plan, objects)
                page_texts = self._vision_page_texts(plan, objects) if source_kind == "vision" else []
                restore_source: dict[str, Any] = {}
                if isinstance(restore, dict):
                    pending = await connection.fetchrow(
                        "SELECT content, content_hash, metadata, change_type "
                        "FROM document_versions WHERE document_id = $1 AND version_number = $2",
                        document_id, int(restore["candidate_version"]),
                    )
                    if (
                        pending is None or pending["change_type"] != "pending_restore"
                        or pending.get("content") != plan.content
                        or pending.get("content_hash") != content_hash
                    ):
                        raise SpecialSourceUnverifiableError(
                            "pending restore content differs from prepared candidate"
                        )
                    restore_source = _json_object(
                        _json_object(pending["metadata"]).get(SOURCE_MANIFEST_KEY)
                    )
                    if (
                        restore_source.get("source_kind") != source_kind
                        or restore_source.get("source_hash") != plan.source_hash
                        or restore_source.get("content_hash") != content_hash
                        or restore_source.get("index_config") != pinned_index_config
                    ):
                        raise SpecialSourceUnverifiableError(
                            "pending restore source identity differs from candidate"
                        )
                    if source_kind == "text" and (
                        restore_source.get("source_hash") != content_hash
                        or restore_source.get("page_texts") != []
                        or restore_source.get("objects") != {}
                    ):
                        raise SpecialSourceUnverifiableError(
                            "pending text restore source receipt is incomplete"
                        )
                    if source_kind == "vision":
                        saved_pages = restore_source.get("page_texts")
                        if (
                            not isinstance(saved_pages, list)
                            or len(saved_pages) != len(page_texts)
                            or any(not isinstance(page, dict) for page in saved_pages)
                            or [
                            (page.get("page_number"), page.get("text"), page.get("sha256"))
                            for page in saved_pages
                        ] != [
                            (page["page_number"], page["text"], page["sha256"])
                            for page in page_texts
                        ]
                        ):
                            raise SpecialSourceUnverifiableError(
                                "pending restore page receipts differ from candidate"
                            )
                original_source_key = str(
                    (restore_source.get("original_source_key") or "") if restore_source
                    else metadata.get("original_file_key") or ""
                )
                if source_kind == "vision" and not original_source_key:
                    raise SpecialSourceUnverifiableError("vision original source key is unavailable")
                candidate_source = {
                    "generation_id": generation_id,
                    "source_hash": plan.source_hash,
                    "content_hash": content_hash,
                    "source_kind": source_kind,
                    "original_source_key": original_source_key,
                    "index_config": pinned_index_config,
                    "page_texts": page_texts,
                    "objects": objects,
                }
                candidate_metadata = {
                    **(metadata_patch or {}), SOURCE_MANIFEST_KEY: candidate_source,
                }
                if source_kind == "text":
                    # The old special image/level receipts must stop describing
                    # the active document in the same transaction that removes
                    # their segment rows. Historical version receipts stay intact.
                    candidate_metadata.update({
                        "processing_mode": "text_only",
                        "original_file_key": original_source_key,
                        "extracted_images": [], "image_count": 0,
                        "images_embedded": False, "embedded_image_count": 0,
                        "pages_processed": 0, "total_pages": 0,
                        "l1_segments": 0, "l2_segments": 0, "l3_segments": 0,
                        "total_vectors": len(plan.segment_ids),
                        "segments_created": len(plan.segment_ids),
                    })
                current_version = int(current["current_version"] or 0)
                existing = await connection.fetchrow(
                    "SELECT content_hash, content, metadata FROM document_versions "
                    "WHERE document_id = $1 AND version_number = $2",
                    document_id, current_version,
                ) if current_version > 0 else None
                if existing is not None and _sha256_text(str(existing.get("content") or "")) != str(existing.get("content_hash") or ""):
                    raise SpecialSourceUnverifiableError("old document version bytes differ")
                if existing is not None and str(existing["content"] or "") != str(current["content"] or ""):
                    raise SpecialSourceUnverifiableError(
                        "old document version and serving content differ"
                    )
                has_old_points = any(old_ids.values())
                if source_kind == "text":
                    serving_source = _json_object(metadata.get(SOURCE_MANIFEST_KEY))
                    if not serving_source:
                        raise SpecialSourceUnverifiableError(
                            "old special serving source receipt is unavailable"
                        )
                    version_source = _json_object(
                        _json_object(existing.get("metadata")).get(SOURCE_MANIFEST_KEY)
                    ) if existing is not None else {}
                    if version_source and version_source != serving_source:
                        raise SpecialSourceUnverifiableError(
                            "old special version source differs from serving source"
                        )
                baseline_needed = existing is None and has_old_points
                if isinstance(restore, dict) and existing is None:
                    raise SpecialSourceUnverifiableError("restore has no active old version")
                old_source = None
                if existing is not None or baseline_needed:
                    old_source = await self._old_source_manifest(
                        {**dict(current), "content": existing["content"] if existing else current["content"]},
                        document_id, tenant_id=tenant_id,
                        index_config=pinned_index_config, connection=connection,
                    )
                if isinstance(restore, dict):
                    version_number = int(restore["candidate_version"])
                    previous_version = int(restore["previous_version"])
                    create_version = False
                else:
                    previous_version = None
                    # A new generation gets a version even when its text is
                    # identical: image objects and source metadata may differ.
                    create_version = True
                    next_version = int(await connection.fetchval(
                        "SELECT COALESCE(MAX(version_number), 0) + 1 "
                        "FROM document_versions WHERE document_id = $1", document_id,
                    ))
                    version_number = next_version + int(baseline_needed)
                for points in plan.points_by_collection.values():
                    for point in points:
                        point.payload = {
                            **(point.payload or {}), "tenant_id": tenant_id,
                            "dataset_id": dataset_id, "document_id": document_id,
                            "source_version": version_number, "source_hash": content_hash,
                        }
                for row in plan.segment_rows:
                    row["metadata"] = {
                        **_json_object(row.get("metadata")),
                        "source_version": version_number, "source_hash": content_hash,
                    }
                if source_kind == "text":
                    # Segment upsert preserves an operator disable by
                    # (document, content type, position). Match the Qdrant
                    # candidate's visibility before writing either store.
                    disabled_positions = await self.db.get_operator_disabled_segment_positions(
                        document_id, "text", connection=connection,
                    )
                    disabled_ids = {
                        str(row["segment_id"]) for row in plan.segment_rows
                        if int(row["position"]) in disabled_positions
                    }
                    for points in plan.points_by_collection.values():
                        for point in points:
                            if str(point.id) in disabled_ids:
                                point.payload = {**(point.payload or {}), "enabled": False}
                plan_hash = self._plan_hash(plan, objects, candidate_source)
                manifest = await self.db.advance_special_publication_manifest(
                    execution_id, document_id, dataset_id, generation_id,
                    source_hash=plan.source_hash, plan_hash=plan_hash,
                    expected_phase="preparing", next_phase="prepared",
                    collections=collections, objects=objects,
                )
                lexical = LexicalConfig.from_index_config(dataset.get("index_config") or {})
                for name, points in sorted(plan.points_by_collection.items()):
                    if not points:
                        continue
                    vector = points[0].vector
                    if not isinstance(vector, list):
                        raise RuntimeError("special candidate requires dense vectors")
                    options = (
                        {"lexical_config": lexical} if name == base and lexical.configured
                        else {"lexical_config": LexicalConfig(), "allow_lexical_transition": True}
                        if name != base else {}
                    )
                    await self.vector_store.ensure_collection(
                        dataset_id=dataset_id, dimension=len(vector),
                        collection_name=name, tenant_id=tenant_id,
                        lifecycle_lease_held=True, **options,
                    )
                    await self.vector_store.upsert(
                        name, points, expected_ingestion_identity=identity,
                        lifecycle_lease_held=True,
                    )
                manifest = await self.db.advance_special_publication_manifest(
                    execution_id, document_id, dataset_id, generation_id,
                    source_hash=plan.source_hash, plan_hash=plan_hash,
                    expected_phase="prepared", next_phase="points_written",
                )
                async with connection.transaction():
                    await self.db.commit_text_segment_publication(
                        dataset_id=dataset_id, document_id=document_id,
                        segment_rows=plan.segment_rows,
                        keep_segment_ids=list(plan.segment_ids),
                        staged_segment_ids=list(plan.segment_ids),
                        delete_excess=True, delete_all_excess=True,
                        expected_ingestion_identity=identity,
                        connection=connection, finish_publication=False,
                        candidate_content=plan.content, expected_content=expected_content,
                        candidate_metadata_patch=candidate_metadata,
                        candidate_word_count=len(plan.content.split()),
                        candidate_version_number=version_number if restore else None,
                        previous_version_number=previous_version,
                        finalize_document=True,
                        defer_terminal_until_fence_release=True,
                    )
                    if getattr(plan, "summary_row", None):
                        saved = await self.db.save_document_summary(
                            plan.summary_row, connection=connection,
                        )
                        if not saved:
                            raise RuntimeError("special candidate summary could not be saved")
                    else:
                        await self.db.delete_document_summary(
                            document_id, connection=connection,
                        )
                    if old_source is not None:
                        if baseline_needed:
                            baseline_metadata = {
                                **metadata, SOURCE_MANIFEST_KEY: old_source,
                            }
                            baseline = await self.db.create_document_version(
                                document_id, str(current["content"] or ""),
                                _sha256_text(str(current["content"] or "")),
                                "created" if current_version == 0 else "updated",
                                title=str(current["title"] or ""),
                                metadata=baseline_metadata, connection=connection,
                                activate=False,
                            )
                            if baseline is None or int(baseline["version_number"]) != version_number - 1:
                                raise RuntimeError("old serving source snapshot changed")
                        else:
                            await self._set_version_manifest(
                                connection, document_id, current_version, old_source,
                            )
                    if create_version:
                        version = await self.db.create_document_version(
                            document_id, plan.content, content_hash,
                            "created" if int(current["current_version"] or 0) == 0 else "updated",
                            title=str(current["title"] or ""),
                            metadata=candidate_metadata, connection=connection,
                        )
                        if version is None or int(version["version_number"]) != version_number:
                            raise RuntimeError("special candidate version number changed")
                    else:
                        await self._set_version_manifest(
                            connection, document_id, version_number, candidate_source,
                        )
                    await connection.execute(
                        """UPDATE documents
                           SET version_count = (
                               SELECT COUNT(*) FROM document_versions
                               WHERE document_id = $1
                           )
                           WHERE document_id = $1""",
                        document_id,
                    )
                    manifest = await self.db.advance_special_publication_manifest(
                        execution_id, document_id, dataset_id, generation_id,
                        source_hash=plan.source_hash, plan_hash=plan_hash,
                        expected_phase="points_written", next_phase="committed",
                        connection=connection,
                    )
                committed = True
                await self._close_committed(
                    {"execution_id": execution_id, "document_id": document_id,
                     "dataset_id": dataset_id, **manifest},
                    publication.revision, connection, active_context,
                    tenant_id=tenant_id,
                )
                return list(plan.segment_ids)
            except BaseException:
                if not committed:
                    owner = await self.db.get_special_publication_manifest(execution_id)
                    if owner is not None and owner.get("phase") != "committed":
                        await self._abort_uncommitted(
                            owner, publication.revision, connection, active_context,
                            tenant_id=tenant_id,
                        )
                # A committed PG generation needs its old-ID cleanup completed
                # by recover_unfinished; keep its negative revision otherwise.
                raise

    async def abort_preparing(
        self, dataset: dict[str, Any], execution_id: str,
        generation_id: str, source_hash: str, *,
        document_shared_lease_held: bool = False,
        resume_same_execution: bool = False,
    ) -> bool:
        """Retire a failed preparation using only its predeclared object keys."""

        dataset_id = str(dataset.get("dataset_id") or "")
        tenant_id = str(dataset.get("tenant_id") or "")
        owner = await self.db.get_special_publication_manifest(execution_id)
        if (
            not dataset_id or not tenant_id or owner is None
            or owner.get("dataset_id") != dataset_id
            or owner.get("generation_id") != generation_id
            or owner.get("source_hash") != source_hash
            or owner.get("phase") != "preparing"
        ):
            raise RuntimeError("failed preparation has no matching durable owner")

        def require_active(active: dict[str, Any]) -> None:
            if (
                active.get("execution_status") != "running"
                or active.get("execution_id") != execution_id
                or active.get("dataset_id") != dataset_id
                or active.get("document_id") != owner["document_id"]
                or active.get("generation_id") != generation_id
                or active.get("source_hash") != source_hash
                or active.get("phase") != "preparing"
            ):
                raise RuntimeError("failed preparation lost its unique active owner")

        # Before the first negative revision there is no revision-bound dataset
        # owner yet. Check the single execution and document marker, then let
        # the lease atomically bind this execution to its negative revision.
        document = await self.db.get_document(owner["document_id"])
        document_metadata = _json_object((document or {}).get("metadata"))
        if (
            owner.get("execution_status") != "running"
            or not document or document.get("dataset_id") != dataset_id
            or document_metadata.get("_document_pipeline_execution_id") != execution_id
            or document_metadata.get("_special_publication_generation_id") != generation_id
        ):
            raise RuntimeError("failed preparation lost its document owner")
        base = str(dataset.get("collection_name") or "")
        await self._active_context(dataset_id, base)
        async with self.db.dataset_index_publication_lease(
            dataset_id, expected_ingestion_identity=dataset_ingestion_identity(dataset),
            special_execution_id=execution_id,
            document_shared_lease_held=document_shared_lease_held,
        ) as publication:
            require_active(await self.db.get_active_special_publication_for_dataset(
                dataset_id, connection=publication.connection,
            ))
            active_context = await self._active_context(dataset_id, base)
            current = await self.db.get_special_publication_manifest(
                execution_id, connection=publication.connection,
            )
            immutable = (
                "execution_id", "document_id", "dataset_id", "execution_status",
                "generation_id", "source_hash", "planned_object_keys", "phase",
            )
            if (
                current is None
                or any(current.get(key) != owner.get(key) for key in immutable)
                or current.get("publication_revision") != abs(publication.revision)
            ):
                raise RuntimeError("failed preparation manifest changed before cleanup")
            if resume_same_execution:
                await self._resume_preparing(
                    current, publication.revision, publication.connection,
                    active_context, tenant_id=tenant_id,
                )
            else:
                await self._abort_uncommitted(
                    current, publication.revision, publication.connection,
                    active_context, tenant_id=tenant_id,
                )
            return True

    async def recover_unfinished(self, dataset: dict[str, Any]) -> bool:
        """Resolve a recovered negative revision from its durable manifest."""

        dataset_id = str(dataset.get("dataset_id") or "")
        tenant_id = str(dataset.get("tenant_id") or "")
        if not dataset_id or not tenant_id:
            raise ValueError("special recovery dataset owner is incomplete")
        current = await self.db.get_dataset(dataset_id)
        if current is None or int(current.get("content_revision") or 0) >= 0:
            return False
        async with self.db.dataset_index_publication_lease(
            dataset_id, expected_ingestion_identity=dataset_ingestion_identity(dataset),
        ) as publication:
            if not publication.recovered:
                raise RuntimeError("special recovery lost its negative revision")
            owner = await self.db.get_active_special_publication_for_dataset(
                dataset_id, connection=publication.connection,
            )
            active_context = await self._active_context(
                dataset_id, str(dataset.get("collection_name") or ""),
            )
            if owner["phase"] == "committed":
                await self._close_committed(
                    owner, publication.revision, publication.connection, active_context,
                    tenant_id=tenant_id,
                )
            elif owner["phase"] == "preparing":
                await self._resume_preparing(
                    owner, publication.revision, publication.connection,
                    active_context, tenant_id=tenant_id,
                )
            elif owner["phase"] in {"prepared", "points_written", "aborted"}:
                await self._abort_uncommitted(
                    owner, publication.revision, publication.connection, active_context,
                    tenant_id=tenant_id,
                )
            else:
                raise RuntimeError("special recovery manifest phase is invalid")
            return True
